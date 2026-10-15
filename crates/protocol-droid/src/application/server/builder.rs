//! [`Server`] — the builder a user of this crate actually touches.
//!
//! # What it is
//!
//! A server is a [`RegistryStore`], a `Vec` of tool handlers, and the
//! negotiation inputs a connection will need. It is a value, not a running
//! process: nothing here binds a port or spawns a task. T13 and T14 supply the
//! transports that take one of these and give it somewhere to run, and until
//! then a test can drive it in-process.
//!
//! # Why a builder and not a `new`
//!
//! A server needs a name before it needs anything else, and most of what
//! follows is optional. A `Server::new(name, tools, resources, prompts)` is
//! four positional arguments in an order nobody can hold in their head, and it
//! is wrong in five of the six shapes a caller wants. The builder makes the
//! order free and names each thing at the call site.

use std::num::NonZeroUsize;
use std::sync::Arc;

use crate::adapters::secondary::idempotency::InMemoryIdempotencyStore;
use crate::application::idempotency::IdempotencyStore;
use crate::application::registry::store::RegistryStore;
use crate::application::server::context::RequestContext;
use crate::domain::idempotency::IdempotencyError;
use crate::domain::ir::{IrErrorCode, JsonObject};
use crate::domain::registry::{
    Primitive, PrimitiveId, PrimitiveKind, Prompt, Registered, Resource, ResourceTemplate, Tool,
};

/// A tool handler, and what it answers with.
///
/// The boxed form rather than a generic parameter, deliberately: a server holds
/// a heterogeneous set of these behind one name, so there is nothing to
/// monomorphise over. The alternative — a generic `Server<H>` — buys nothing
/// here and makes the struct unnameable in a field.
///
/// The [`Send`] bound is load-bearing. A handler runs on the runtime, and a
/// handler that is not `Send` cannot be sent to it.
pub type ToolHandler = Arc<dyn Fn(RequestContext, JsonObject) -> BoxFuture + Send + Sync>;

/// The future a [`ToolHandler`] returns.
///
/// Its own alias because spelling `Pin<Box<dyn Future<...> + Send>>` at every
/// call site is how one of them ends up without the `Send`.
pub type BoxFuture = std::pin::Pin<Box<dyn Future<Output = JsonObject> + Send>>;

/// A tool: what it is, and how to run it.
///
/// Handwritten `Debug` rather than derived, for the same reason the session
/// context's is: a `ToolHandler` is a boxed closure, and the derived form
/// prints a vtable pointer and a captured environment that no one reading a
/// log line can use. The name and the schema are the parts that identify it.
struct ToolEntry {
    name: PrimitiveId,
    handler: ToolHandler,
}

impl core::fmt::Debug for ToolEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ToolEntry")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// A server, built.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use protocol_droid::application::server::Server;
/// use protocol_droid::domain::ir::JsonObject;
///
/// let server = Server::builder("demo")
///     .simple_tool(
///         "echo",
///         r#"{"type":"object","properties":{"text":{"type":"string"}}}"#,
///         Arc::new(|_ctx, args| {
///             Box::pin(async move {
///                 // A handler answers with the tool's result object, not with
///                 // a `Result`: MCP reports a tool failure as a successful
///                 // call carrying `isError: true`, so "the tool failed" is
///                 // something the handler *returns* rather than propagates.
///
///                 // The result is built from a `serde_json::Value` so the
///                 // peer's text is escaped by the serialiser. Hand-writing
///                 // the document would make injection a matter of how many
///                 // quotes a caller remembered.
///                 let echoed = args.as_str().to_string();
///                 let result = JsonObject::from_value(serde_json::json!({
///                     "content": [{ "type": "text", "text": echoed }],
///                 }));
///                 result.unwrap_or_default()
///             })
///         }),
///     )
///     .expect("the schema above is valid")
///     .build();
///
/// assert_eq!(server.tool_count(), 1);
/// assert_eq!(server.name(), "demo");
/// assert!(server.handler("echo").is_some());
/// assert!(server.handler("missing").is_none());
/// ```
// Manual `Debug` rather than derived, for the reason `Dispatcher` has one: the
// store is a `dyn` port with no readable state, so the derived form would
// print `idempotency: <unknown>` on every line of a post-mortem and say
// nothing. What a reader wants is the name, the tool count, and whether a
// store was ever supplied — the third because a server still on the default
// has an in-memory bound, and that is the question to ask first when a
// recorded result has gone missing across a restart.
pub struct Server {
    name: String,
    version: String,
    registry: RegistryStore,
    tools: Vec<ToolEntry>,
    /// Deduplicates `tools/call` by request.
    ///
    /// A trait object rather than a concrete store because a caller serving
    /// over HTTP wants a bound that outlives one process — a retried call may
    /// arrive at a different replica — and a concrete `InMemoryIdempotencyStore`
    /// in this field would be the kind of type that compiles everywhere and is
    /// only correct on the one host. The default is the in-memory store, which
    /// is correct for a single process and honest about being so.
    idempotency: Arc<dyn IdempotencyStore<Result = JsonObject>>,
    /// The type-keyed state handlers read through
    /// [`RequestContext::state`](crate::application::server::context::RequestContext::state).
    ///
    /// `Arc` so a `Server` is cheap to clone into a `Dispatcher`, and
    /// the dispatcher is what the transport's per-request handler
    /// actually reaches into. An empty map is the default; a server
    /// that never called [`ServerBuilder::with_state`] gets an empty
    /// map and `state::<T>()` returns `None` for every `T`.
    state: Arc<crate::application::server::context::StateMap>,
}

impl core::fmt::Debug for Server {
    /// Prints the name, the version, and the tool count.
    ///
    /// Not the store, and the omission is deliberate: a built server always
    /// has one, so printing that would say nothing. Whether a store was
    /// *supplied* is a question about the builder, and the builder's own
    /// `Debug` is where it is answerable.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Server")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("tool_count", &self.tools.len())
            .finish_non_exhaustive()
    }
}

impl Server {
    /// Serves this server on stdin and stdout until the peer closes stdin.
    ///
    /// The shortest complete serving path in the crate, and the one the
    /// documentation elsewhere assumes exists. The revision is settled from the
    /// peer's own `initialize`, so this neither takes a version nor forces the
    /// caller to pick one — which is the whole reason the crate exists.
    ///
    /// The caller does not have to know that a [`Dispatcher`] and a
    /// [`PeerSession`] are involved, or that the session's profile is resolved
    /// before the loop starts. Both stay reachable: this is a façade over the
    /// types above, not a replacement for them.
    ///
    /// [`Dispatcher`]: crate::application::server::dispatch::Dispatcher
    /// [`PeerSession`]: crate::application::server::context::PeerSession
    ///
    /// # Errors
    ///
    /// Propagates a [`ServeError`] for a read, write, or diagnostics failure.
    /// A protocol error is not one of those: a peer whose frame this server
    /// cannot serve is answered on the wire rather than raised here.
    ///
    /// [`ServeError`]: crate::adapters::primary::stdio::ServeError
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use protocol_droid::application::server::Server;
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let server = Server::builder("greeter").build();
    /// let report = server.serve_stdio().await?;
    /// eprintln!("{report:?}");
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "stdio")]
    pub async fn serve_stdio(
        self,
    ) -> Result<
        crate::adapters::primary::stdio::ServeReport,
        crate::adapters::primary::stdio::ServeError,
    > {
        let mut loop_ = crate::application::server::serve::StdioBuilder::new(self).build();
        loop_.serve().await
    }

    /// Serves this server over Streamable HTTP, on loopback, until it stops.
    ///
    /// The same one-call shape as [`serve_stdio`](Self::serve_stdio), for the
    /// same reason. The alternative — building an `McpRouter`, an
    /// `axum::Router`, and a listener — moves the work onto the consumer
    /// rather than removing it, and the pieces a consumer would have to reach
    /// for are a `SessionStore` with no production `IdSource` in it and a
    /// `RequestHandler` with no implementation for this `Dispatcher` anywhere
    /// but the tests.
    ///
    /// Binds `127.0.0.1:3000` and serves plain HTTP. That is a development
    /// shape, not a deployment one: a non-loopback address and TLS in front of
    /// it are the caller's to arrange, and [`HttpBuilder`] is how.
    ///
    /// # Errors
    ///
    /// [`ServeError`] from building the route table, from binding, or from the
    /// accept loop. A protocol error is not one of those: a peer whose request
    /// this server cannot serve is answered on the wire rather than raised
    /// here.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use protocol_droid::application::server::Server;
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let server = Server::builder("greeter").build();
    /// let report = server.serve_http().await?;
    /// eprintln!("listening on {}", report.addr);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`HttpBuilder`]: crate::adapters::primary::transport::http::HttpBuilder
    /// [`ServeError`]: crate::adapters::primary::transport::http::ServeError
    #[cfg(feature = "http")]
    pub async fn serve_http(
        self,
    ) -> Result<
        crate::adapters::primary::transport::http::ServeReport,
        crate::adapters::primary::transport::http::ServeError,
    > {
        crate::adapters::primary::transport::http::HttpBuilder::new(self)
            .serve()
            .await
    }

    /// Starts a server with a name and nothing else.
    ///
    /// Every method on the returned [`ServerBuilder`] is optional, so this
    /// alone is a valid — if useless — server. That is the honest minimum: a
    /// server with no tools is a real state, not a mistake to prevent at the
    /// type level.
    #[must_use]
    pub fn builder(name: impl Into<String>) -> ServerBuilder {
        ServerBuilder {
            name: name.into(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            registry: RegistryStore::new(),
            tools: Vec::new(),
            idempotency: None,
            state: Vec::new(),
        }
    }

    /// The name this server was built with.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The version this server reports in `serverInfo`.
    ///
    /// Free-form on the wire — the schema types it as a plain `string` — so
    /// nothing constrains the format, only that a peer can read it. It lives
    /// beside [`name`](Self::name) because the two are the same fact: they are
    /// the two fields of the `serverInfo` object, they are both about who this
    /// server is rather than what it does, and splitting them would mean a
    /// server whose `name` came from the builder and whose `version` came from
    /// somewhere else.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// How many tools are registered, hidden or not.
    #[must_use]
    pub const fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// The store `tools/call` is deduplicated through.
    ///
    /// Exposed so a transport can answer "have you seen this id before?"
    /// without running the tool, which is what a resuming peer wants: it has
    /// dropped a connection and wants to know whether the call it never saw
    /// answered actually happened.
    #[must_use]
    pub const fn idempotency(&self) -> &Arc<dyn IdempotencyStore<Result = JsonObject>> {
        &self.idempotency
    }

    /// Registers a state value after the server is built.
    ///
    /// The builder API is `ServerBuilder::with_state`; this method
    /// exists for callers that built a server through a path that did
    /// not surface the builder (the `#[server]` macro's
    /// `into_server` is the canonical one). Each call appends to
    /// the existing map, so a server that already had state for one
    /// type can grow state for a second.
    ///
    /// `Send + Sync + 'static` is what every other port in this crate
    /// asks for; a pool of non-`Send` handles cannot live in this
    /// map.
    pub fn with_state<T: Send + Sync + 'static>(&mut self, value: T) {
        Arc::make_mut(&mut self.state).insert(
            std::any::TypeId::of::<T>(),
            Arc::new(value) as Arc<dyn std::any::Any + Send + Sync>,
        );
    }

    /// The state map handlers read through
    /// [`RequestContext::state`](crate::application::server::context::RequestContext::state).
    ///
    /// A transport hands this to every [`RequestContext`](crate::application::server::context::RequestContext)
    /// it builds. A handler that never calls `with_state` on the
    /// builder gets an empty map here, and `state::<T>()` returns
    /// `None` for every `T`.
    #[must_use]
    pub fn state(&self) -> &Arc<crate::application::server::context::StateMap> {
        &self.state
    }

    /// The registry, for the transport that serves it.
    ///
    /// Takes `&self` and hands out an owned snapshot later, so a handler
    /// publishing a new tool does not need the server borrowed mutably while
    /// a request is in flight.
    #[must_use]
    pub const fn registry(&self) -> &RegistryStore {
        &self.registry
    }

    /// The handler for `name`, if there is one.
    ///
    /// Takes the tool out under a lock-free read, so a concurrent
    /// registration cannot invalidate the borrow.
    #[must_use]
    pub fn handler(&self, name: &str) -> Option<&ToolHandler> {
        self.tools
            .iter()
            .find(|entry| entry.name.as_str() == name)
            .map(|entry| &entry.handler)
    }

    /// Finds `name` and runs it.
    ///
    /// The lookup and the call together rather than a `handler()` the caller
    /// has to remember to use: a transport and an in-process test that each
    /// wrote their own lookup would drift, and the drift would show up as a
    /// tool that is in the registry and not callable.
    ///
    /// Visibility is decided by the **registry**, and only by the registry. It
    /// used to be a second, cheaper check over a cached `required` set held by
    /// each tool entry — which covered features and not scopes. So a
    /// scope-restricted tool was absent from `tools/list` and still ran on
    /// `tools/call`, which is the existence oracle
    /// [`UnknownTool`](DispatchError::UnknownTool) exists to prevent.
    ///
    /// The cached set is gone rather than kept as a belt-and-braces second
    /// check. Once the registry gate runs first, that check can only ever
    /// agree with it, so it decides nothing — and a check that decides nothing
    /// is a check that can drift into deciding the wrong thing without anyone
    /// noticing. One predicate for the whole surface is the property worth
    /// having.
    ///
    /// # Errors
    ///
    /// [`DispatchError::UnknownTool`] when no tool of that name is registered
    /// **or** when this session may not see it — because it requires a
    /// feature the profile lacks, or a scope the caller does not hold. The two
    /// are deliberately the same answer: a different one for each would let a
    /// caller read the existence of a tool it is not allowed to know about.
    /// The server still logs which it was.
    pub async fn call(
        &self,
        ctx: &RequestContext,
        name: &str,
        args: JsonObject,
    ) -> Result<JsonObject, DispatchError> {
        let entry = self
            .tools
            .iter()
            .find(|entry| entry.name.as_str() == name)
            .ok_or(DispatchError::UnknownTool)?;

        // The gate, once, for both halves of the surface. After this the tool
        // is known to exist *and* to be visible to this session, so the
        // handler is reached with no further checking to disagree with.
        if self
            .registry
            .get_visible(
                ctx.peer().auth(),
                &ctx.peer().profile(),
                PrimitiveKind::Tool,
                name,
            )
            .is_none()
        {
            return Err(DispatchError::UnknownTool);
        }

        Ok((entry.handler)(ctx.clone(), args).await)
    }
}

/// Why a tool call did not run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DispatchError {
    /// No tool of that name is registered.
    ///
    /// Deliberately the same answer for a tool that exists but is hidden from
    /// this session: the difference would let a client probe for the existence
    /// of a tool it may not see. The server still logs which it was.
    #[error("no such tool")]
    UnknownTool,

    /// The tool is registered, but the session did not negotiate the features
    /// it requires.
    #[error("the session did not negotiate what this tool requires")]
    NotCallable,

    /// No method of that name exists.
    ///
    /// Refused on the method, not on a tool, because the caller named a method
    /// and there is no tool to say anything about.
    #[error("no such method")]
    UnknownMethod,

    /// `tools/call` params were not the shape a call has to be: not an object,
    /// no string `name`, or an `arguments` that is not an object.
    ///
    /// A distinct variant from `UnknownTool` because a call with no `name` at
    /// all has not failed to find a tool — it never named one, and answering
    /// "no such tool" would send the caller looking for a tool that works.
    #[error("the call parameters are not a tool call")]
    MalformedParams,

    /// `initialize` params were not the shape the handshake has to be.
    ///
    /// Separate from [`MalformedParams`](Self::MalformedParams) because the
    /// two failures are not the same event: that one says a tool call named
    /// nothing, and this one says the one request that establishes a session
    /// arrived in a shape that would have to be guessed at. A peer sending it
    /// has a bug worth naming, not a tool to go looking for.
    ///
    /// Covers a body that is not an object, a missing or non-string
    /// `protocolVersion`, a string that is not a protocol version, a missing or
    /// non-object `capabilities`, and a `clientInfo` with no `name`.
    #[error("the initialize parameters are not a handshake")]
    MalformedInitialize,

    /// The session is on a revision this build has no wire types for.
    ///
    /// Distinct from a refused *method* ([`UnknownMethod`](Self::UnknownMethod))
    /// because nothing about the request was wrong: the method exists, the
    /// params parse, and the answer cannot be built because the revision the
    /// session settled on is one [`negotiate_server`] would have to counter-
    /// offer. The session should never be in this state — it is reported
    /// rather than papered over, because the alternative is emitting a result
    /// whose shape the revision cannot express.
    #[error("the session is on a revision this build cannot serve")]
    UnnegotiableVersion,

    /// A second request arrived with an id this session is still working on.
    ///
    /// The JSON-RPC reading, and the reason it is an error rather than a
    /// queue: the first request is a real, running, side-effecting call, and
    /// the second carries no way to say what should happen to it. Running the
    /// tool twice is the answer a retry gets by accident, and this is the
    /// answer it gets on purpose. A client that wants the first result waits
    /// for it and retries later — at which point the id is `Completed` and
    /// comes straight back.
    #[error("request {} is already in flight on this session", .0)]
    DuplicateInFlight(String),

    /// The store holding recorded results could not be reached.
    ///
    /// Reported rather than swallowed because the alternative is the one thing
    /// this whole path exists to prevent: a store that errors, a caller that
    /// treats the error as "no claim", and a tool that runs a second time.
    /// Failing the call is the only answer that does not charge a client twice
    /// behind its back.
    #[error("recorded results are unavailable: {0}")]
    IdempotencyUnavailable(String),
}

impl DispatchError {
    /// The JSON-RPC code this refusal is reported as.
    ///
    /// Lives here rather than in a transport adapter because it is a property
    /// of the error, not of the way it is written down. It sat in the stdio
    /// adapter until the HTTP router needed the same mapping, and the two
    /// options at that point were both wrong: copy it and let the copies drift
    /// apart, or reach into another adapter and break the layering.
    ///
    /// Every refusal gets an answer. The alternative is to drop it, which
    /// leaves the peer waiting on a response that is never coming — and a
    /// server that hangs is worse than one that says no. The codes are the
    /// closest the standard offers: a method nobody serves is `-32601`, a call
    /// that does not name something callable is `-32602`, and a session that
    /// did not negotiate a feature — or one settled on a revision this build
    /// cannot serve — is a server-side condition rather than either.
    ///
    /// A malformed `initialize` is `-32602` like any other malformed params:
    /// the peer sent a request whose body it could not have meant, which is the
    /// same condition whatever the method was.
    ///
    /// # Examples
    ///
    /// ```
    /// # use protocol_droid::application::server::DispatchError;
    /// # use protocol_droid::domain::ir::IrErrorCode;
    /// assert_eq!(
    ///     DispatchError::UnknownMethod.jsonrpc_code(),
    ///     IrErrorCode::MethodNotFound,
    /// );
    /// assert_eq!(
    ///     DispatchError::MalformedParams.jsonrpc_code(),
    ///     IrErrorCode::InvalidParams,
    /// );
    /// ```
    #[must_use]
    pub const fn jsonrpc_code(&self) -> IrErrorCode {
        match self {
            Self::UnknownMethod => IrErrorCode::MethodNotFound,
            Self::MalformedParams | Self::MalformedInitialize | Self::UnknownTool => {
                IrErrorCode::InvalidParams
            }
            // A request that names an id this session is already working on is
            // a frame that violates JSON-RPC, so it takes the code for a frame
            // the specification does not permit — not the code for a tool it
            // cannot run, which is a different failure with a different fix.
            Self::DuplicateInFlight(_) => IrErrorCode::InvalidRequest,
            // A store that cannot be reached is this server's problem, whatever
            // the peer sent, so it is a server-side code. Reporting it as the
            // caller's fault would send a client looking for a bug in its own
            // request when the recording of results is what failed.
            Self::NotCallable | Self::UnnegotiableVersion | Self::IdempotencyUnavailable(_) => {
                IrErrorCode::InternalError
            }
        }
    }

    /// The code and message pair a transport writes to the wire.
    ///
    /// The message is [`thiserror`]'s own for the variant, never the method
    /// name the peer chose: a method name is a JSON string a peer controls, and
    /// JSON permits a newline inside one, so echoing it into a log line would
    /// let a peer close that line and open one of its own.
    #[must_use]
    pub fn as_wire(&self) -> (IrErrorCode, String) {
        (self.jsonrpc_code(), self.to_string())
    }
}

impl From<IdempotencyError> for DispatchError {
    /// Keeps a poisoned or absent store from reading as an absent claim.
    fn from(error: IdempotencyError) -> Self {
        Self::IdempotencyUnavailable(error.to_string())
    }
}

/// The builder behind [`Server::builder`].
pub struct ServerBuilder {
    name: String,
    version: String,
    registry: RegistryStore,
    tools: Vec<ToolEntry>,
    idempotency: Option<Arc<dyn IdempotencyStore<Result = JsonObject>>>,
    /// The state entries built up through `with_state` calls. Held as a
    /// `Vec<(TypeId, Arc<dyn Any>)>` and re-keyed at build time because
    /// the `TypeId` of `T` is only known to `with_state`, and pushing
    /// into a `HashMap` keyed by a value the caller cannot see is the
    /// wrong shape for a builder.
    state: Vec<(std::any::TypeId, Arc<dyn std::any::Any + Send + Sync>)>,
}

impl core::fmt::Debug for ServerBuilder {
    /// Prints the same three facts as [`Server`], plus whether a store was
    /// supplied.
    ///
    /// The last is the one worth having here, and it is the question to ask
    /// first when a recorded result has gone missing across a restart: a
    /// server still on the default has an in-process bound, and no way to see
    /// that from the built server it became.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ServerBuilder")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("tool_count", &self.tools.len())
            .field("idempotency_supplied", &self.idempotency.is_some())
            .finish_non_exhaustive()
    }
}

impl ServerBuilder {
    /// Sets the version this server reports in `serverInfo`.
    ///
    /// Defaults to the version of the crate that built it, which is right for
    /// a server that *is* this crate — the examples, the conformance runner —
    /// and wrong for one that merely links it. A library user shipping their
    /// own server should say so here rather than let the dependency's version
    /// stand in for theirs, which would have every downstream server claiming
    /// to be whatever `protocol-droid` happened to be pinned at.
    #[must_use]
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Replaces the store `tools/call` is deduplicated through.
    ///
    /// For a store that outlives the process — a shared cache, a table — or
    /// for a test that wants to assert on what was recorded. A server that
    /// keeps its own `IdSource` across restarts has a gap the in-memory
    /// default cannot close: a call recorded before a restart is unknown
    /// after it, and the retry runs the tool again.
    #[must_use]
    pub fn idempotency(mut self, store: Arc<dyn IdempotencyStore<Result = JsonObject>>) -> Self {
        self.idempotency = Some(store);
        self
    }

    /// Registers a value handlers can read through
    /// [`RequestContext::state`](crate::application::server::context::RequestContext::state).
    ///
    /// `T` is keyed by [`TypeId`], so two `with_state` calls with the same
    /// `T` keep the later value (the registry is a `HashMap`, and the
    /// later write is the one the lookup will see). The `Send + Sync`
    /// bounds are the same every other port in this crate asks for; a
    /// pool of non-`Send` handles cannot live in this map.
    ///
    /// The point of this method is the `#[tool]` problem: a macro that
    /// generates module-scope items cannot see `self`, so the handler
    /// it generates has no captured environment. Storing the state
    /// here and reading it through the request context is the
    /// standard answer, and the only one that keeps the macro
    /// out of the receiver business.
    #[must_use]
    pub fn with_state<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.state.push((
            std::any::TypeId::of::<T>(),
            Arc::new(value) as Arc<dyn std::any::Any + Send + Sync>,
        ));
        self
    }

    /// Adds a tool, and publishes it to the registry in the same step.
    ///
    /// Takes a whole [`Tool`] rather than a name and a schema string, because
    /// a schema string can only ever express the plainest tool there is. An
    /// output schema, annotations and icons all change what the tool
    /// _requires_ of a session, and a builder that cannot name them would
    /// register a tool whose requirement nothing had computed.
    ///
    /// Registering and publishing together is the point: a tool in the list
    /// but not in the registry is a tool a client can see and not call, and
    /// one in neither is a handler that never runs. There is no state in
    /// between for a caller to be wrong in.
    #[must_use]
    pub fn tool(self, tool: Tool, handler: ToolHandler) -> Self {
        self.restricted_tool(Registered::from(Primitive::from(tool)), handler)
    }

    /// Adds a tool that carries its own requirements.
    ///
    /// [`tool`](Self::tool) takes a [`Tool`], and a `Tool` cannot say
    /// "callers need the `files:read` scope" — that lives on [`Registered`],
    /// which wraps a primitive with the policy it must satisfy. So without this
    /// method the scope half of `is_visible` is reachable from no builder
    /// call, `auth.rs` is dead code, and a server can be built that has no way
    /// to express an authorisation rule at all. That is the same dead-gate
    /// shape T11 found, one layer up.
    ///
    /// A `Registered` carrying something other than a tool is registered
    /// under its own id and left out of the call table: it stays visible to
    /// `tools/list` and the listing is the truth, so nothing is silently
    /// dropped, but there is no handler to name for a `tools/call` of it.
    #[must_use]
    pub fn restricted_tool(mut self, registered: Registered, handler: ToolHandler) -> Self {
        if let Primitive::Tool(tool) = registered.primitive() {
            self.tools.push(ToolEntry {
                name: tool.name.clone(),
                handler,
            });
        }
        self.registry.register(registered);
        self
    }

    /// Adds a plain tool: a name, an input schema, and a handler.
    ///
    /// The common case, and the one a tool without an output schema or
    /// annotations fits. It exists so the common case is one line rather than
    /// a `Tool::new(...)?` at every call site, and it returns a `Result`
    /// because the schema is the one fallible step.
    /// # Errors
    ///
    /// The [`JsonObject`](crate::domain::ir::JsonObjectError) from parsing
    /// `input_schema`. The schema is validated here, once, so a malformed one
    /// fails at build time rather than on the first call.
    pub fn simple_tool(
        self,
        name: impl Into<PrimitiveId>,
        input_schema: &str,
        handler: ToolHandler,
    ) -> Result<Self, crate::domain::ir::JsonObjectError> {
        Ok(self.tool(Tool::new(name, input_schema)?, handler))
    }

    /// Adds a resource.
    ///
    /// Publishes to the registry rather than holding a side list, for the same
    /// reason `tool` does: a resource in a builder field but not in the
    /// registry is a resource no `resources/list` can name. The field it used
    /// to live in was dropped by `build`, so a server with resources had a
    /// builder that looked populated and an always-empty listing.
    #[must_use]
    pub fn resource(self, resource: Resource) -> Self {
        self.registry
            .register(Registered::from(Primitive::from(resource)));
        self
    }

    /// Adds a resource template.
    ///
    /// Templates are listed by their own method, and they are a distinct
    /// [`PrimitiveKind`] in the registry, so a template needs its own entry
    /// point rather than sharing `resource`'s.
    #[must_use]
    pub fn resource_template(self, template: ResourceTemplate) -> Self {
        self.registry
            .register(Registered::from(Primitive::from(template)));
        self
    }

    /// Adds a prompt.
    #[must_use]
    pub fn prompt(self, prompt: Prompt) -> Self {
        self.registry
            .register(Registered::from(Primitive::from(prompt)));
        self
    }

    /// Finishes the server.
    #[must_use]
    pub fn build(self) -> Server {
        // Deduplicating by default rather than on request. A caller who has
        // never heard of idempotency is the caller most likely to be serving
        // a retriable HTTP transport, where a dropped connection turns one
        // call into two, and a store that has to be opted into is a store
        // nobody will think to opt into. The default is the bound that fails
        // safe: a server that forgets runs a tool once rather than twice.
        let idempotency = self.idempotency.unwrap_or_else(|| {
            // 4096 keys is generous for a single process and small enough to
            // bound memory. The capacity is a policy number, not a constant
            // with a meaning, so it is named rather than inlined.
            const DEFAULT_KEYS: NonZeroUsize = match NonZeroUsize::new(4096) {
                Some(value) => value,
                // Unreachable: 4096 is not zero. A `match` rather than an
                // `unwrap` because the crate denies that.
                None => NonZeroUsize::MIN,
            };
            Arc::new(InMemoryIdempotencyStore::with_capacity(DEFAULT_KEYS))
        });
        Server {
            name: self.name,
            version: self.version,
            registry: self.registry,
            tools: self.tools,
            idempotency,
            state: Arc::new(
                self.state
                    .into_iter()
                    .collect::<std::collections::HashMap<_, _>>(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{DispatchError, Server};
    use crate::application::server::context::{PeerSession, RequestContext};
    use crate::application::session::SinkError;
    use crate::domain::feature::FeatureProfile;
    use crate::domain::ir::{JsonObject, JsonObjectError};
    use crate::domain::registry::PrimitiveKind;
    use crate::domain::session::{CancellationToken, SessionId};
    use crate::domain::{ProtocolVersion, RequestId, TransportKind};

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

    /// A handler that answers with a fixed object, so a test can tell one tool
    /// from another without every case writing its own closure.
    fn answering(answer: &str) -> Result<super::ToolHandler, TestError> {
        let answer = JsonObject::new(answer)?;
        Ok(Arc::new(move |_ctx: RequestContext, _args: JsonObject| {
            let answer = answer.clone();
            Box::pin(async move { answer })
        }))
    }

    /// A port pair that answers nothing and records nothing.
    ///
    /// The builder tests never elicit, sample, or emit progress — they are
    /// about registration and dispatch — so the ports exist only to satisfy
    /// `PeerSession`. A `RefCell` is safe here precisely because nothing
    /// touches them, and a real channel would be a second thing to get wrong
    /// in a test that is not about channels.
    struct Inert;

    impl crate::application::session::FrameSink for Inert {
        fn send(&self, _frame: &str) -> impl Future<Output = Result<(), SinkError>> + Send {
            std::future::ready(Ok(()))
        }
    }

    impl crate::application::server::context::CallbackExchange for Inert {
        fn exchange(
            &self,
            _method: &str,
            _params: &JsonObject,
        ) -> impl Future<Output = Result<JsonObject, crate::application::server::ExchangeError>> + Send
        {
            std::future::ready(Err(crate::application::server::ExchangeError::new(
                "inert in these tests",
            )))
        }
    }

    fn peer_at(profile: FeatureProfile) -> PeerSession {
        PeerSession::new(
            ProtocolVersion::V2025_06_18,
            profile,
            SessionId::new(1),
            TransportKind::Stdio,
            Arc::new(Inert),
            Arc::new(Inert),
        )
    }

    fn ctx_for(profile: FeatureProfile) -> RequestContext {
        RequestContext::for_request(
            &peer_at(profile),
            RequestId::number(1),
            CancellationToken::new(),
            None,
        )
    }

    /// A tool added to the builder is in the registry in the same step.
    ///
    /// The builder's claim is that there is no state in between — no tool that
    /// is listable but not callable. A registry the builder has to be told
    /// about separately cannot make that claim, so this is the test of the
    /// claim and not a restatement of the code.
    #[test]
    fn a_registered_tool_is_in_the_registry_immediately() -> Result<(), TestError> {
        let server = Server::builder("demo")
            .simple_tool("echo", r#"{"type":"object"}"#, answering("{}")?)?
            .build();

        assert_eq!(server.tool_count(), 1);
        assert_eq!(server.registry().snapshot().len(PrimitiveKind::Tool), 1);
        Ok(())
    }

    /// A malformed input schema fails at build time, not on the first call.
    #[test]
    fn a_malformed_input_schema_is_rejected_at_build_time() -> Result<(), TestError> {
        // The assertion is on the `Result` itself rather than on an error
        // message: the error's text is not the contract, the timing of the
        // failure is. A test that unwrapped and matched on a string would pass
        // against a builder that validated for the wrong reason.
        let handler = answering("{}")?;
        let built = Server::builder("demo").simple_tool("bad", "{not json", handler);

        match built {
            Ok(_) => Err(Failure::from(
                "a malformed input schema was accepted at build time",
            )),
            Err(_) => Ok(()),
        }
    }

    /// Two tools with one name is one tool, not two.
    ///
    /// T11's store treats a re-registration as a replacement, and the builder
    /// has to agree or the registry would list a name the dispatcher cannot
    /// resolve to a single handler.
    #[test]
    fn a_second_tool_of_the_same_name_replaces_the_first() -> Result<(), TestError> {
        let server = Server::builder("demo")
            .simple_tool(
                "echo",
                r#"{"type":"object"}"#,
                answering(r#"{"first":true}"#)?,
            )?
            .simple_tool(
                "echo",
                r#"{"type":"object"}"#,
                answering(r#"{"second":true}"#)?,
            )?
            .build();

        assert_eq!(server.tool_count(), 2, "both handlers are kept");
        assert_eq!(
            server.registry().snapshot().len(PrimitiveKind::Tool),
            1,
            "but the registry holds one name"
        );
        Ok(())
    }

    /// An unknown tool is refused without reaching any handler.
    #[tokio::test]
    async fn an_unknown_tool_is_refused() -> Result<(), TestError> {
        let server = Server::builder("demo")
            .simple_tool("echo", r#"{"type":"object"}"#, answering("{}")?)?
            .build();
        let ctx = ctx_for(FeatureProfile::all());

        match server.call(&ctx, "nope", JsonObject::new("{}")?).await {
            Ok(answer) => Err(Failure::from(format!("expected a refusal, got {answer}"))),
            Err(DispatchError::UnknownTool) => Ok(()),
            Err(other) => Err(Failure::from(format!(
                "refused for the wrong reason: {other}"
            ))),
        }
    }

    /// A registered tool whose required features the session lacks is refused
    /// by `call`, not run and refused afterwards.
    ///
    /// A tool with an output schema requires `ToolStructuredOutput`. A session
    /// that never negotiated it must not run the tool and then drop the part
    /// of the answer the client could not have used.
    #[tokio::test]
    async fn a_tool_needing_an_absent_feature_is_not_run() -> Result<(), TestError> {
        use crate::domain::registry::Tool;

        // The full `Tool`, not `simple_tool`: an output schema is what raises
        // the requirement in the first place, and `simple_tool` could not
        // express one. A test written against it would be testing a gap.
        let structured = Tool::new("echo", r#"{"type":"object"}"#)?
            .with_output_schema(r#"{"type":"object"}"#)?;

        let server = Server::builder("demo")
            .tool(structured, answering(r#"{"ran":true}"#)?)
            .build();

        let ctx = ctx_for(FeatureProfile::empty());
        match server.call(&ctx, "echo", JsonObject::new("{}")?).await {
            Ok(answer) => Err(Failure::from(format!(
                "a tool the session cannot call returned {answer}"
            ))),
            // `UnknownTool`, not `NotCallable`, and the distinction is the
            // point. A tool this session may not see is a tool this session is
            // not told about: answering "it exists but you may not have it"
            // would confirm it exists, and a caller could then enumerate the
            // registry by watching which names come back refused. The same
            // answer as a tool that is not there is what makes the refusal
            // probe-proof.
            Err(DispatchError::UnknownTool) => Ok(()),
            Err(other) => Err(Failure::from(format!(
                "refused for the wrong reason: {other}"
            ))),
        }
    }
}
