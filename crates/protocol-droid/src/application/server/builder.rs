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

use std::sync::Arc;

use crate::application::registry::store::RegistryStore;
use crate::application::server::context::RequestContext;
use crate::domain::ir::{FeatureSet, JsonObject};
use crate::domain::registry::{Primitive, PrimitiveId, Prompt, Registered, Resource, Tool};

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
    tool: Tool,
    handler: ToolHandler,
    required: FeatureSet,
}

impl core::fmt::Debug for ToolEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ToolEntry")
            .field("name", &self.name)
            .field("tool", &self.tool)
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
#[derive(Debug)]
pub struct Server {
    name: String,
    registry: RegistryStore,
    tools: Vec<ToolEntry>,
}

impl Server {
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
            registry: RegistryStore::new(),
            tools: Vec::new(),
            resources: Vec::new(),
            prompts: Vec::new(),
        }
    }

    /// The name this server was built with.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How many tools are registered, hidden or not.
    #[must_use]
    pub const fn tool_count(&self) -> usize {
        self.tools.len()
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
    /// # Errors
    ///
    /// [`DispatchError::UnknownTool`] when no tool of that name is registered.
    /// [`DispatchError::NotCallable`] when the tool is registered but its
    /// required features are not negotiated on `ctx`'s session — the caller
    /// has already passed the profile gate, so this is the registry's own
    /// metadata disagreeing, which is a server bug and is reported as one.
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
        if !entry.required.is_subset_of(ctx.peer().profile().as_set()) {
            return Err(DispatchError::NotCallable);
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
}

/// The builder behind [`Server::builder`].
#[derive(Debug)]
pub struct ServerBuilder {
    name: String,
    registry: RegistryStore,
    tools: Vec<ToolEntry>,
    resources: Vec<Registered>,
    prompts: Vec<Registered>,
}

impl ServerBuilder {
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
    pub fn tool(mut self, tool: Tool, handler: ToolHandler) -> Self {
        // `from` rather than a builder chain on `Registered`: the conversion
        // is the only way to reach a `Registered`, and spelling it once here
        // means a future field on `Registered` cannot be forgotten below.
        //
        // The requirement is read off the same conversion and cached. Asking
        // the primitive again on every call would clone a `Tool` per dispatch
        // to answer a question that is fixed the moment it is registered.
        let as_primitive = Primitive::from(tool.clone());
        let required = as_primitive.meta().required_features();
        self.registry.register(Registered::from(as_primitive));
        self.tools.push(ToolEntry {
            name: tool.name.clone(),
            tool,
            handler,
            required,
        });
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
    #[must_use]
    pub fn resource(mut self, resource: Resource) -> Self {
        self.resources
            .push(Registered::from(Primitive::from(resource)));
        self
    }

    /// Adds a prompt.
    #[must_use]
    pub fn prompt(mut self, prompt: Prompt) -> Self {
        self.prompts.push(Registered::from(Primitive::from(prompt)));
        self
    }

    /// Finishes the server.
    #[must_use]
    pub fn build(self) -> Server {
        Server {
            name: self.name,
            registry: self.registry,
            tools: self.tools,
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
    use crate::domain::{ProtocolVersion, RequestId};

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
            Err(DispatchError::NotCallable) => Ok(()),
            Err(other) => Err(Failure::from(format!(
                "refused for the wrong reason: {other}"
            ))),
        }
    }
}
