//! [`Client`] — the calling side of a negotiated session.
//!
//! # What it is
//!
//! A client holds the revisions it will speak, in preference order, and the
//! features it wants. It does not open a connection: a transport does that, and
//! until T13 supplies one, a test can hand the client a closure directly.
//!
//! # The gate, and where it lives
//!
//! Every typed method consults the resolved [`FeatureProfile`] before it does
//! anything, exactly as the server's
//! [`RequestContext`](crate::application::server::RequestContext) does. The
//! asymmetry is deliberate: the server's gate protects the _peer_ from a
//! feature the server did not negotiate, and the client's gate protects the
//! _caller_ from a round trip that cannot work.
//!
//! Both produce the same [`ClientError`], because from the caller's side
//! "this session cannot do that" is one fact with one fix — configure a
//! version that can, or drop the call.

use std::sync::Arc;

use crate::domain::TransportSpec;
use crate::domain::feature::{Feature, FeatureProfile, Negotiation};
use crate::domain::ir::JsonObject;
use crate::domain::{
    CapabilitySet, NegotiatedSession, ProtocolVersion, SupportedVersions, TransportKind,
};

pub mod bearer;
mod connect;
pub mod handler;
mod handshake;
pub mod inputs;

pub use connect::{ConnectError, default_versions, open};

pub use handshake::{
    DISCOVER, HandshakeError, INITIALIZE, INITIALIZED, discover_session, handshake,
    initialize_params, initialize_session, offer,
};

/// Sends one request to the peer and returns its answer.
///
/// The port a client needs, and the only one. A transport implements it; the
/// client does not know about sockets, headers, or framing.
///
/// The boxed form because a client's exchange is chosen at run time from a
/// configuration and a test substitutes a different one — there is no
/// monomorphisation to win here, and holding a `dyn` is the honest shape.
pub trait ClientTransport: Send + Sync {
    /// Sends `method` with `params` and yields the peer's answer.
    ///
    /// # Errors
    ///
    /// [`TransportError`] when the request could not be delivered or answered.
    /// A client that waited forever would be a caller that cannot be
    /// interrupted, so a non-answer is an error rather than a hang.
    fn send(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>>;

    /// Sends `method` with `params` and expects no answer.
    ///
    /// Separate from [`send`](Self::send) because a notification is not a
    /// request that tolerates a missing reply — it has no `id` at all, and a
    /// transport that waited for one would wait forever, because a conforming
    /// peer correctly sends nothing. The `notifications/initialized` that ends
    /// the handshake is one of these, so a client that could not send it had no
    /// way to finish connecting.
    ///
    /// # Errors
    ///
    /// [`TransportError`] when the notification could not be written. Nothing
    /// else is possible: there is no reply to be wrong about.
    fn notify(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>>;
}

/// Why a request did not complete.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The transport could not deliver the request, or the peer did not
    /// answer it.
    ///
    /// A `String` for the same reason the server's
    /// [`ExchangeError`](crate::application::server::ExchangeError) is: the
    /// transport names its own failures and the application layer has no
    /// business enumerating them.
    #[error("{0}")]
    Unavailable(String),

    /// The peer answered, and the answer was a JSON-RPC error.
    ///
    /// Separate from [`Unavailable`](Self::Unavailable) because the peer is
    /// fine and the request was not: a caller can act on the code. SEP-2575's
    /// `-32022` carries the revisions the server does speak, and `-32021` the
    /// capabilities it needs, and a client that flattened them to a string
    /// could neither renegotiate nor retry.
    #[error("the peer refused with {code}: {message}")]
    Refused {
        /// The JSON-RPC error code.
        code: i64,
        /// The peer's message.
        message: String,
        /// The error's `data` member as JSON text, when there was one.
        data: Option<String>,
    },
}

impl TransportError {
    /// The revisions a `-32022` refusal says the server supports.
    #[must_use]
    pub fn supported_versions(&self) -> Option<Vec<String>> {
        let Self::Refused {
            code: -32022,
            data: Some(data),
            ..
        } = self
        else {
            return None;
        };
        let parsed: serde_json::Value = serde_json::from_str(data).ok()?;
        Some(
            parsed
                .get("supported")?
                .as_array()?
                .iter()
                .filter_map(|version| version.as_str().map(ToOwned::to_owned))
                .collect(),
        )
    }
}

/// Why a client call did not happen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// The negotiated session does not have the feature the call needs.
    ///
    /// The revision is carried for the same reason the server's is: "sampling
    /// is unavailable" is not something a caller can act on without knowing
    /// what _was_ negotiated.
    #[error("{feature} was not negotiated (session is {negotiated})")]
    FeatureUnavailable {
        /// The feature the call needed.
        feature: Feature,
        /// The revision this session settled on.
        negotiated: ProtocolVersion,
    },

    /// The request could not be delivered or answered.
    #[error("the request did not complete: {0}")]
    Transport(#[from] TransportError),

    /// The server asked for input and nothing could provide it.
    ///
    /// Names the callback so a one-shot caller sees what to wire rather than
    /// a hang: an agent loop that waits for a person who is not there is worse
    /// than one that fails.
    #[error("the server asked for `{method}` input, which no resolver provided")]
    InputUnresolved {
        /// The input request's method, e.g. `elicitation/create`.
        method: String,
    },

    /// The server kept asking for input past the round limit.
    #[error("the server asked for input more than {0} times")]
    TooManyRounds(u32),
}

impl ClientError {
    /// The feature this refusal is about, when there is one.
    #[must_use]
    pub const fn feature(&self) -> Option<Feature> {
        match self {
            Self::FeatureUnavailable { feature, .. } => Some(*feature),
            Self::Transport(_) | Self::InputUnresolved { .. } | Self::TooManyRounds(_) => None,
        }
    }
}

/// What failed when [`Client::connect_stdio`] could not open a session.
///
/// Mirrors [`ConnectError`] but is stdio-specific: the spawn stage is the
/// one the HTTP paths do not have, and the typed inner errors are the
/// adapter's spawn / handshake / versions errors rather than the HTTP
/// ones. Kept as a distinct type so a caller that handles "could not
/// connect" once does not have to know which transport it was.
#[cfg(feature = "stdio")]
#[derive(Debug, thiserror::Error)]
pub enum StdioConnectError {
    /// The offered revisions could not be built.
    #[error("this build's own revision table is unusable: {0}")]
    Versions(#[from] crate::domain::SupportedVersionsError),

    /// The transport config could not be built.
    #[error("the transport config is not usable: {0}")]
    Config(#[from] ClientError),

    /// The child could not be spawned.
    #[error("the child could not be spawned: {0}")]
    Spawn(#[from] crate::adapters::secondary::stdio::SpawnError),

    /// The handshake did not complete.
    #[error("the handshake did not complete: {0}")]
    Handshake(#[from] HandshakeError),
}

/// What a client offers a server, before anything is negotiated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// The revisions this client speaks, most preferred first.
    ///
    /// An ordered list rather than one version because a client that can speak
    /// three revisions should say so and take the best the server agrees to,
    /// rather than pick one and fail when the server prefers another. A
    /// single-element list is the "I only speak this" case and needs no
    /// separate type.
    versions: Vec<ProtocolVersion>,
    capabilities: CapabilitySet,
    transport: TransportKind,
}

impl ClientConfig {
    /// A client that speaks `version`, with nothing advertised.
    #[must_use]
    pub fn new(version: ProtocolVersion, transport: TransportKind) -> Self {
        Self {
            versions: vec![version],
            capabilities: CapabilitySet::empty(),
            transport,
        }
    }

    /// A client that speaks several, most preferred first.
    ///
    /// # Errors
    ///
    /// Refuses an empty list, because a client that speaks no revision cannot
    /// negotiate and would otherwise fail much later with a message about
    /// something else entirely.
    pub fn speaking(
        versions: impl IntoIterator<Item = ProtocolVersion>,
        transport: TransportKind,
    ) -> Result<Self, ClientError> {
        let versions: Vec<ProtocolVersion> = versions.into_iter().collect();
        if versions.is_empty() {
            return Err(ClientError::Transport(TransportError::Unavailable(
                "a client must offer at least one revision".to_string(),
            )));
        }
        Ok(Self {
            versions,
            capabilities: CapabilitySet::empty(),
            transport,
        })
    }

    /// Declares the capabilities the client supports.
    #[must_use]
    pub const fn with_capabilities(mut self, capabilities: CapabilitySet) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// The revisions offered, most preferred first.
    #[must_use]
    pub fn versions(&self) -> &[ProtocolVersion] {
        &self.versions
    }

    /// The capabilities this client declared.
    ///
    /// Read by the handshake to render the `initialize` request, so it has to
    /// be the same set the profile will be resolved from — a handshake that
    /// advertised a different one would produce a session that claims to do
    /// something it never told the server it could.
    #[must_use]
    pub const fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    /// The transport the client will run over.
    #[must_use]
    pub const fn transport(&self) -> TransportKind {
        self.transport
    }

    /// The negotiation this offer produces against a server's answer.
    ///
    /// The one place the two halves are put together. A caller that already
    /// knows the version and the server's capabilities — which is what a
    /// transport knows once the handshake is done — passes them here rather
    /// than leaving this type to invent a half-negotiation.
    #[must_use]
    pub const fn negotiated_with(
        &self,
        version: ProtocolVersion,
        server_caps: CapabilitySet,
    ) -> Negotiation {
        Negotiation {
            version,
            client_caps: self.capabilities,
            server_caps,
            transport: self.transport,
        }
    }
}

/// A client bound to a negotiated session.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use protocol_droid::application::client::{Client, ClientConfig, TransportError};
/// use protocol_droid::domain::ir::JsonObject;
/// use protocol_droid::domain::{ProtocolVersion, TransportKind};
///
/// /// A transport that always refuses, which is enough to show the gate.
/// struct Dead;
/// # impl protocol_droid::application::client::ClientTransport for Dead {
/// #     fn send(&self, _: &str, _: &JsonObject)
/// #         -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send>>
/// #     { Box::pin(async { Err(TransportError::Unavailable("no route".to_string())) }) }
/// #     fn notify(&self, _: &str, _: &JsonObject)
/// #         -> std::pin::Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send>>
/// #     { Box::pin(async { Err(TransportError::Unavailable("no route".to_string())) }) }
/// # }
///
/// let version = ProtocolVersion::V2024_11_05;
/// let config = ClientConfig::new(version, TransportKind::Stdio);
/// let client = Client::connect(
///     config.negotiated_with(version, protocol_droid::domain::CapabilitySet::all_for(version)),
///     Arc::new(Dead),
/// );
///
/// // Tools exist in every revision, and both sides declared everything that
/// // revision defines, so the gate is open and the dead transport is what
/// // fails the call.
/// assert!(client.supports(protocol_droid::domain::Feature::Tools));
///
/// let outcome = tokio::runtime::Builder::new_current_thread()
///     .build()
///     .map_err(|e| e.to_string())?
///     .block_on(client.list_tools());
/// assert_eq!(
///     outcome.map_or_else(|e| e.to_string(), |_| "succeeded".to_string()),
///     "the request did not complete: no route",
/// );
/// # Ok::<(), String>(())
/// ```
#[derive(Clone)]
pub struct Client {
    session: Arc<NegotiatedSession>,
    transport: Arc<dyn ClientTransport>,
}

impl core::fmt::Debug for Client {
    /// Prints the negotiation, not the transport.
    ///
    /// For the same reason `PeerSession` does: a transport is a socket and a
    /// vtable pointer, and neither survives being printed at a call site.
    ///
    /// The session is printed as its version and feature count rather than in
    /// full because it carries both peers' `PeerInfo`, which is caller-supplied
    /// text and does not belong in a log line nobody asked for.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Client")
            .field("negotiated", &self.session.version)
            .field("transport", &self.session.transport)
            .field("feature_count", &self.session.profile.len())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Binds a transport to a session that has already negotiated.
    ///
    /// Takes the whole [`Negotiation`] rather than just the version. The
    /// profile is a function of four inputs, and a client knows only two of
    /// them — its own capabilities and the transport. Re-deriving it here
    /// would mean guessing the server's half, and a guess that leaves the
    /// server's capabilities empty turns a feature nobody declared into a
    /// feature nobody may use. The transport did the handshake and has the
    /// real answer, so it passes the real answer.
    ///
    /// Prefer [`from_session`](Self::from_session) when a
    /// [`NegotiatedSession`] is to hand: it is this constructor minus the copy,
    /// and without the copy's chance of being wrong. This one exists for a
    /// caller assembling a [`Negotiation`] directly, where re-deriving is the
    /// only way to get a profile at all.
    #[must_use]
    pub fn connect(negotiation: Negotiation, transport: Arc<dyn ClientTransport>) -> Self {
        Self {
            session: Arc::new(NegotiatedSession::new(
                negotiation.version,
                negotiation.transport,
                negotiation.client_caps,
                negotiation.server_caps,
            )),
            transport,
        }
    }

    /// Binds a transport to a session that has already negotiated.
    ///
    /// The one to reach for. [`connect`](Self::connect) takes the four inputs
    /// [`resolve`](crate::domain::feature::resolve) needs and re-derives a profile from them — but a
    /// [`NegotiatedSession`] has _already_ resolved one, from those same four
    /// fields, so a caller going through `connect` rebuilds a struct literal
    /// and re-runs a pure function to arrive back where it started.
    ///
    /// That is worse than redundant. Nothing obliges the four fields a caller
    /// copies to be the four the session was built from, so a caller who
    /// transposes one — or leaves `client_caps` empty because the transport
    /// made it awkward — gets a client whose profile disagrees with the session
    /// it just negotiated, and the disagreement surfaces much later as a
    /// feature that was advertised and then refused. Reading the profile off
    /// the session removes the copy, and with it the possibility of copying it
    /// wrong.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use protocol_droid::application::client::{Client, ClientConfig, ClientTransport, handshake};
    /// use protocol_droid::domain::{ProtocolVersion, TransportKind};
    ///
    /// async fn connect(transport: Arc<dyn ClientTransport>) -> Result<(), String> {
    ///     let config = ClientConfig::speaking(
    ///         [ProtocolVersion::V2025_06_18].into_iter(),
    ///         TransportKind::Stdio,
    ///     )
    ///     .map_err(|e| e.to_string())?;
    ///     let session = handshake(transport.as_ref(), &config)
    ///         .await
    ///         .map_err(|e| e.to_string())?;
    ///     let client = Client::from_session(&session, transport);
    ///     assert!(client.supports(protocol_droid::domain::Feature::Tools));
    ///     Ok(())
    /// }
    /// ```
    #[must_use]
    pub fn from_session(session: &NegotiatedSession, transport: Arc<dyn ClientTransport>) -> Self {
        Self {
            session: Arc::new(session.clone()),
            transport,
        }
    }

    /// Connects to `spec`, negotiates, and returns a bound client.
    ///
    /// The one-call form, for the common case where the caller has a URL and no
    /// opinion about revisions. The four steps it replaces — pick the adapter,
    /// choose what to offer, run the handshake, bind the result — are all
    /// decisions the crate can make from the spec, and a caller who has to make
    /// them is doing the crate's work.
    ///
    /// Offers every revision this build has wire types for. Narrow that with
    /// [`connect_to_speaking`](Self::connect_to_speaking) when the server is
    /// known to be older, or when a build is expected to run against something
    /// that answers a narrow offer badly.
    ///
    /// # Errors
    ///
    /// [`ConnectError`] naming the stage that failed. A transport this build
    /// has no adapter for is [`ConnectError::Unsupported`] rather than a
    /// handshake error, so "this crate cannot speak that" is never reported as
    /// "that server would not talk".
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use protocol_droid::application::client::Client;
    /// # use protocol_droid::domain::TransportSpec;
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let spec = TransportSpec::new(
    ///     protocol_droid::domain::TransportKind::StreamableHttp,
    ///     "http://127.0.0.1:3000/mcp",
    /// )?;
    /// let client = Client::connect_to(&spec).await?;
    /// let tools = client.list_tools().await?;
    /// # let _ = tools;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect_to(spec: &TransportSpec) -> Result<Self, ConnectError> {
        Ok(connect::open(spec, &default_versions()?).await?.0)
    }

    /// Connects to `spec`, offering exactly `versions`.
    ///
    /// [`connect_to`](Self::connect_to) for a caller who has an opinion about
    /// what to offer. The narrowing is deliberate: offering a revision a server
    /// does not speak gets the *next* one it does, so a set that excludes the
    /// right revision is refused by servers that would otherwise have worked.
    ///
    /// # Errors
    ///
    /// As [`connect_to`](Self::connect_to), plus
    /// [`ConnectError::Config`] when `versions` is empty.
    pub async fn connect_to_speaking(
        spec: &TransportSpec,
        versions: &SupportedVersions,
    ) -> Result<Self, ConnectError> {
        Ok(connect::open(spec, versions).await?.0)
    }

    /// Connects to `spec`, offering `versions` and applying `options`.
    ///
    /// For a client that can answer the server: a server asks for sampling,
    /// elicitation or roots only when the client declared them, so
    /// [`connect_to`](Self::connect_to), which declares nothing, can never be
    /// asked. The handler in `options` answers what the server does ask.
    ///
    /// # Errors
    ///
    /// As [`connect_to_speaking`](Self::connect_to_speaking).
    pub async fn connect_with(
        spec: &TransportSpec,
        versions: &SupportedVersions,
        options: handler::ConnectOptions,
    ) -> Result<Self, ConnectError> {
        Ok(connect::open_with(spec, versions, options).await?.0)
    }

    /// Connects to an MCP server over stdio by spawning it as a child.
    ///
    /// `spec` is the program and (optionally) arguments; `args` is
    /// appended after the program's own arguments. The function spawns
    /// the child, drains the child's stderr into
    /// `Diagnostics::stderr()`, runs the MCP `initialize` handshake,
    /// and binds the resulting session to a [`Client`].
    ///
    /// This is the stdio counterpart of [`connect_to`](Self::connect_to)
    /// for the HTTP transports. The four steps the consumer would
    /// otherwise write by hand — spawn a [`StdioClient`], build a
    /// [`ClientConfig`], run [`handshake`], bind the session with
    /// [`from_session`](Self::from_session) — are one call here.
    /// Each of those steps is a decision the consumer should not be
    /// making for a use case the library already has an answer to:
    /// which adapter speaks a given spec is a fact about the crate,
    /// the set of revisions to offer is a matter of what this build
    /// has wire types for, and the diagnostics channel is the obvious
    /// one. A consumer who has an opinion on any of those uses the
    /// four steps directly; both paths are public.
    ///
    /// # Errors
    ///
    /// Returns [`StdioConnectError`] on any of the four stages. The
    /// child is killed on failure, so a refused handshake does not
    /// leave a process behind.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use protocol_droid::adapters::secondary::stdio::ChildSpec;
    /// use protocol_droid::application::client::Client;
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let spec = ChildSpec::new("upstream-mcp-server")?;
    /// let client = Client::connect_stdio(&spec).await?;
    /// let tools = client.list_tools().await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`StdioClient`]: crate::adapters::secondary::stdio::StdioClient
    /// [`ClientConfig`]: crate::adapters::secondary::stdio::ClientConfig
    #[cfg(feature = "stdio")]
    pub async fn connect_stdio(
        spec: &crate::adapters::secondary::stdio::ChildSpec,
    ) -> Result<Self, StdioConnectError> {
        use crate::adapters::primary::stdio::Diagnostics;
        use crate::adapters::secondary::stdio::StdioClient;

        let diagnostics = Diagnostics::stderr();
        let transport_config = crate::adapters::secondary::stdio::ClientConfig::default();
        let transport = StdioClient::spawn(spec, transport_config, diagnostics)
            .await
            .map_err(StdioConnectError::Spawn)?;
        let transport: Arc<dyn ClientTransport> = Arc::new(transport);
        let versions = default_versions().map_err(StdioConnectError::Versions)?;
        let config = ClientConfig::speaking(versions.as_slice().to_vec(), TransportKind::Stdio)
            .map_err(StdioConnectError::Config)?;
        let session = handshake(transport.as_ref(), &config)
            .await
            .map_err(StdioConnectError::Handshake)?;
        Ok(Self::from_session(&session, transport))
    }

    /// The revision this client and its server agreed on.
    #[must_use]
    pub fn negotiated(&self) -> ProtocolVersion {
        self.session.version
    }

    /// The profile that revision produced.
    #[must_use]
    pub fn profile(&self) -> FeatureProfile {
        self.session.profile
    }

    /// The negotiated session behind this client.
    ///
    /// Everything the handshake agreed on, including the parts a caller
    /// usually wants for reporting rather than for gating: what the server
    /// called itself and which revision it answered with, what each side
    /// advertised, which transport this is, and whether the profile is
    /// recomputed per request with nothing retained.
    ///
    /// The accessors above answer the two questions a caller asks before every
    /// call. This answers the one asked once, usually by a log line or a
    /// `/healthz` body — and it answers it from the session rather than from a
    /// second copy of the parts a caller remembered to keep.
    ///
    /// Not `const`: the session is behind an `Arc`, and reading through one is
    /// not a constant expression. A caller who needs the version in a `const`
    /// should negotiate in a function it can run, not reach for one here.
    #[must_use]
    pub fn session(&self) -> &NegotiatedSession {
        &self.session
    }

    /// Whether `feature` may be used on this session.
    #[must_use]
    pub fn supports(&self, feature: Feature) -> bool {
        self.session.profile.allows(feature)
    }

    /// Sends `method` exactly as given, skipping the capability gate.
    ///
    /// The typed methods refuse what the server did not advertise, which is
    /// right for an application and wrong for a tool whose job is to see what a
    /// server actually does: an inspector, a prober, a conformance client. A
    /// server may serve a method it did not advertise, and the only way to
    /// learn that is to ask.
    ///
    /// # Errors
    ///
    /// [`ClientError::Transport`] as for any request.
    pub async fn send_unchecked(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Result<JsonObject, ClientError> {
        Ok(self.transport.send(method, params).await?)
    }

    /// Lists the server's tools.
    ///
    /// # Errors
    ///
    /// [`ClientError::FeatureUnavailable`] when the session has no
    /// [`Feature::Tools`], having written nothing. A 2024-11-05 session always
    /// has one, but a session whose peer advertises nothing still might not,
    /// and a caller learns which from this rather than from an opaque
    /// protocol error.
    pub async fn list_tools(&self) -> Result<JsonObject, ClientError> {
        self.require(Feature::Tools)?;
        self.request("tools/list", "{}").await
    }

    /// Calls `name` with `arguments`.
    ///
    /// # Errors
    ///
    /// As [`list_tools`](Self::list_tools).
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &JsonObject,
    ) -> Result<JsonObject, ClientError> {
        self.require(Feature::Tools)?;
        // `arguments` is a validated `JsonObject`, so re-parsing it cannot
        // fail. It has to be re-parsed at all: `JsonObject` is opaque text by
        // design, and nesting one object inside another is the one operation
        // that needs its structure back. The fallback is the empty object
        // rather than an error, because a value that arrived here as a
        // `JsonObject` has already been checked once and a second failure
        // would report the same malformation from two places.
        let nested = serde_json::from_str::<serde_json::Value>(arguments.as_str())
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
        let params = JsonObject::from_value(serde_json::json!({
            "name": name,
            "arguments": nested,
        }))
        .unwrap_or_default();
        self.transport
            .send("tools/call", &params)
            .await
            .map_err(ClientError::from)
    }

    /// Calls `name`, answering any input the server asks for along the way.
    ///
    /// See [`call_resolving`](inputs::call_resolving) for the rules.
    ///
    /// # Errors
    ///
    /// As [`call_tool`](Self::call_tool), plus
    /// [`ClientError::InputUnresolved`] and [`ClientError::TooManyRounds`].
    pub async fn call_tool_resolving(
        &self,
        name: &str,
        arguments: &JsonObject,
        resolve: &(impl inputs::InputResolver + ?Sized),
    ) -> Result<JsonObject, ClientError> {
        self.require(Feature::Tools)?;
        let nested = serde_json::from_str::<serde_json::Value>(arguments.as_str())
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
        let params =
            JsonObject::from_value(serde_json::json!({ "name": name, "arguments": nested }))
                .unwrap_or_default();
        inputs::call_resolving(self.transport.as_ref(), "tools/call", &params, resolve).await
    }

    /// Lists the server's resources.
    ///
    /// # Errors
    ///
    /// As [`list_tools`](Self::list_tools), with
    /// [`Feature::Resources`](Feature) in place of
    /// [`Feature::Tools`](Feature).
    pub async fn list_resources(&self) -> Result<JsonObject, ClientError> {
        self.require(Feature::Resources)?;
        self.request("resources/list", "{}").await
    }

    /// Lists the server's prompts.
    ///
    /// # Errors
    ///
    /// As [`list_tools`](Self::list_tools), with
    /// [`Feature::Prompts`](Feature) in place of
    /// [`Feature::Tools`](Feature).
    pub async fn list_prompts(&self) -> Result<JsonObject, ClientError> {
        self.require(Feature::Prompts)?;
        self.request("prompts/list", "{}").await
    }

    /// Lists the server's resource templates.
    ///
    /// # Errors
    ///
    /// As [`list_resources`](Self::list_resources).
    pub async fn list_resource_templates(&self) -> Result<JsonObject, ClientError> {
        self.require(Feature::Resources)?;
        self.request("resources/templates/list", "{}").await
    }

    /// Reads the resource at `uri`.
    ///
    /// # Errors
    ///
    /// As [`list_resources`](Self::list_resources).
    pub async fn read_resource(&self, uri: &str) -> Result<JsonObject, ClientError> {
        self.require(Feature::Resources)?;
        let params = JsonObject::from_value(serde_json::json!({ "uri": uri })).unwrap_or_default();
        Ok(self.transport.send("resources/read", &params).await?)
    }

    /// Gets the prompt `name`, rendered with `arguments`.
    ///
    /// # Errors
    ///
    /// As [`list_prompts`](Self::list_prompts).
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: &JsonObject,
    ) -> Result<JsonObject, ClientError> {
        self.require(Feature::Prompts)?;
        let nested = serde_json::from_str::<serde_json::Value>(arguments.as_str())
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
        let params =
            JsonObject::from_value(serde_json::json!({ "name": name, "arguments": nested }))
                .unwrap_or_default();
        Ok(self.transport.send("prompts/get", &params).await?)
    }

    /// The gate, in one place, called by every typed method.
    fn require(&self, feature: Feature) -> Result<(), ClientError> {
        if self.session.profile.allows(feature) {
            return Ok(());
        }
        Err(ClientError::FeatureUnavailable {
            feature,
            negotiated: self.session.version,
        })
    }

    /// Sends a request whose gate has already been checked.
    async fn request(&self, method: &str, params: &str) -> Result<JsonObject, ClientError> {
        let owned = match JsonObject::new(params) {
            Ok(object) => object,
            Err(error) => {
                return Err(ClientError::Transport(TransportError::Unavailable(
                    format!("{method} was given parameters that are not a JSON object: {error}"),
                )));
            }
        };
        self.transport
            .send(method, &owned)
            .await
            .map_err(ClientError::from)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{Client, ClientConfig, ClientError, ClientTransport, TransportError};
    use crate::domain::feature::Feature;
    use crate::domain::ir::{JsonObject, JsonObjectError};
    use crate::domain::{CapabilitySet, ProtocolVersion, TransportKind};

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

    /// A transport that records what it was asked and answers with a fixed
    /// object.
    struct Recorder {
        calls: Mutex<Vec<String>>,
        answer: Option<JsonObject>,
    }

    impl Recorder {
        fn answering(answer: &str) -> Result<Self, TestError> {
            Ok(Self {
                calls: Mutex::new(Vec::new()),
                answer: Some(JsonObject::new(answer)?),
            })
        }

        fn dead() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                answer: None,
            }
        }

        fn called(&self) -> Vec<String> {
            let calls = self
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            calls.clone()
        }
    }

    impl ClientTransport for Recorder {
        fn send(
            &self,
            method: &str,
            _params: &JsonObject,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>>
        {
            {
                let mut calls = self
                    .calls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                calls.push(method.to_string());
            }
            let outcome = self
                .answer
                .clone()
                .ok_or_else(|| TransportError::Unavailable("nothing listening".to_string()));
            Box::pin(std::future::ready(outcome))
        }

        /// Records the method and answers nothing, which is the whole
        /// difference between this and `send`.
        fn notify(
            &self,
            method: &str,
            _params: &JsonObject,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>>
        {
            self.called();
            {
                let mut calls = self
                    .calls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                calls.push(method.to_string());
            }
            Box::pin(std::future::ready(Ok(())))
        }
    }

    /// A client at `version`, with both sides declaring everything that
    /// revision defines.
    ///
    /// `all_for` rather than a hand-listed set, so a change to the capability
    /// matrix moves this instead of quietly invalidating the premise.
    fn client_at(version: ProtocolVersion, transport: Arc<Recorder>) -> Client {
        let caps = CapabilitySet::all_for(version);
        let config = ClientConfig::new(version, TransportKind::Stdio);
        Client::connect(config.negotiated_with(version, caps), transport)
    }

    /// A gated call that cannot run writes nothing and names the feature.
    ///
    /// The transport is `dead`, so a call that got through would fail with
    /// `Transport` — which is what makes this a test of the gate and not of
    /// the transport. The error variant is the whole assertion.
    #[tokio::test]
    async fn a_gated_call_on_a_session_without_it_never_reaches_the_transport()
    -> Result<(), TestError> {
        // 2024-11-05 has elicitation only in... it does not have it at all.
        // Resolving with empty capabilities on both sides is the honest way to
        // say "this session negotiated nothing optional".
        let transport = Arc::new(Recorder::dead());
        let version = ProtocolVersion::V2024_11_05;
        let config = ClientConfig::new(version, TransportKind::Stdio);
        let client = Client::connect(
            config.negotiated_with(version, CapabilitySet::empty()),
            transport.clone(),
        );

        let refusal = match client.list_prompts().await {
            Ok(answer) => {
                return Err(Failure::from(format!("expected a refusal, got {answer}")));
            }
            Err(error) => error,
        };

        assert_eq!(
            refusal,
            ClientError::FeatureUnavailable {
                feature: Feature::Prompts,
                negotiated: ProtocolVersion::V2024_11_05,
            }
        );
        assert_eq!(refusal.feature(), Some(Feature::Prompts));
        assert!(transport.called().is_empty());
        Ok(())
    }

    /// An ungated call reaches the transport with the right method name.
    ///
    /// The other half of the gate's contract: a test suite of refusals would
    /// pass just as happily against a client that never sends anything.
    #[tokio::test]
    async fn a_permitted_call_reaches_the_transport() -> Result<(), TestError> {
        let version = ProtocolVersion::V2025_06_18;
        let transport = Arc::new(Recorder::answering(r#"{"tools":[]}"#)?);
        let client = client_at(version, transport.clone());

        let answer = match client.list_tools().await {
            Ok(answer) => answer,
            Err(error) => return Err(Failure::from(error.to_string())),
        };

        assert_eq!(answer.as_str(), r#"{"tools":[]}"#);
        assert_eq!(transport.called(), vec!["tools/list".to_string()]);
        Ok(())
    }

    /// Each typed method sends its own method name.
    ///
    /// A copy-paste that sent `tools/list` for a resource listing would pass
    /// every other test here and break only against a real server, so the
    /// names are checked together rather than one call at a time.
    #[tokio::test]
    async fn each_typed_method_sends_its_own_method() -> Result<(), TestError> {
        let version = ProtocolVersion::V2025_06_18;
        let transport = Arc::new(Recorder::answering("{}")?);
        let client = client_at(version, transport.clone());

        for outcome in [
            client.list_tools().await.map(|_| ()),
            client.list_resources().await.map(|_| ()),
            client.list_prompts().await.map(|_| ()),
        ] {
            if let Err(error) = outcome {
                return Err(Failure::from(error.to_string()));
            }
        }

        assert_eq!(
            transport.called(),
            vec![
                "tools/list".to_string(),
                "resources/list".to_string(),
                "prompts/list".to_string(),
            ]
        );
        Ok(())
    }

    /// A client that offers no revisions is refused at construction.
    ///
    /// Before the negotiation, not during it: a client that cannot say what it
    /// speaks should not produce a session at all.
    #[test]
    fn a_client_offering_no_revisions_is_refused() -> Result<(), TestError> {
        let empty: Vec<ProtocolVersion> = Vec::new();
        let config = ClientConfig::speaking(empty, TransportKind::Stdio);

        match config {
            Ok(_) => Err(Failure::from("a client offering no revisions was accepted")),
            Err(_) => Ok(()),
        }
    }

    /// The profile a client reports is the one the handshake produced.
    ///
    /// The point of taking the whole `Negotiation` rather than guessing the
    /// server's half: a client that invented an empty server capability set
    /// would report a profile stricter than the real session, and this pins
    /// the difference.
    #[test]
    fn the_client_reports_the_negotiations_profile() {
        let version = ProtocolVersion::V2025_06_18;
        let transport = Arc::new(Recorder::dead());
        let config = ClientConfig::new(version, TransportKind::Stdio);

        let generous = Client::connect(
            config.negotiated_with(version, CapabilitySet::all_for(version)),
            transport.clone(),
        );
        let stingy = Client::connect(
            config.negotiated_with(version, CapabilitySet::empty()),
            transport,
        );

        assert_eq!(generous.negotiated(), version);
        assert!(
            generous.supports(Feature::Resources),
            "both sides declared everything {version} defines"
        );
        assert!(
            !stingy.supports(Feature::Resources),
            "a server that declared nothing offers no resources"
        );
    }

    /// A client built from a session keeps the session's own profile.
    ///
    /// This is the property `from_session` exists to guarantee, and the reason
    /// it is tested rather than assumed: the client's profile is only equal to
    /// the session's because `from_session` moves the one across instead of
    /// re-deriving it from a copied struct literal. Nothing in the types
    /// stops someone from changing that back, and the symptom would be a
    /// client that refuses a feature the session it came from allowed.
    ///
    /// The comparison is against the session's profile read off the session
    /// itself. Comparing against another `resolve` call would be a third
    /// derivation, which could agree with both by accident and prove nothing
    /// about the path being tested.
    #[test]
    fn a_client_from_a_session_keeps_the_sessions_profile() {
        let version = ProtocolVersion::V2025_06_18;
        let session = crate::domain::NegotiatedSession::new(
            version,
            TransportKind::Stdio,
            CapabilitySet::all_for(version),
            CapabilitySet::all_for(version),
        );
        let expected = session.profile;
        let client = Client::from_session(&session, Arc::new(Recorder::dead()));

        assert_eq!(client.negotiated(), version);
        assert_eq!(
            client.profile(),
            expected,
            "the client's profile must be the session's, not a second derivation of it"
        );
        assert!(client.supports(Feature::Resources));
    }

    /// The half of the handshake a caller reports rather than gates on.
    ///
    /// `a_client_from_a_session_keeps_the_sessions_profile` covers the two
    /// fields the typed methods read, which is why `Client` was able to keep
    /// only those two and pass. This one covers the five the profile says
    /// nothing about, and it is the test that would have caught a client
    /// which answered "which transport is this?" by guessing from the URL.
    ///
    /// Deliberately built on `with_server_info` rather than on the bare
    /// constructor: an `Arc::new(NegotiatedSession::new(..))` would satisfy
    /// every version and capability assertion here while silently dropping the
    /// peer names, and the symptom a consumer hits is a log line that says
    /// "server: unknown" against a server that announced itself twice.
    #[test]
    fn a_client_reports_what_the_handshake_agreed() {
        let version = ProtocolVersion::V2025_06_18;
        let caps = CapabilitySet::all_for(version);
        let session =
            crate::domain::NegotiatedSession::new(version, TransportKind::LegacySse, caps, caps)
                .with_client_info(crate::domain::PeerInfo::new("droid-cli", "0.4.1"))
                .with_server_info(crate::domain::PeerInfo::new("weather", "2.11.0"));

        let client = Client::from_session(&session, Arc::new(Recorder::dead()));
        let seen = client.session();

        assert_eq!(seen.version, version);
        assert_eq!(
            seen.transport,
            TransportKind::LegacySse,
            "the transport comes off the session, not off the URL the caller passed"
        );
        assert_eq!(seen.client_caps, caps);
        assert_eq!(seen.server_caps, caps);
        assert_eq!(
            seen.server_info.as_ref().map(|i| i.name.as_str()),
            Some("weather"),
            "a peer that announced itself must still be able to say so"
        );
        assert_eq!(
            seen.client_info.as_ref().map(|i| i.version.as_str()),
            Some("0.4.1"),
            "our own identity is as reportable as the server's"
        );
        assert!(
            !seen.is_stateless,
            "a 2025-06-18 session retains state; only 2026-07-28 does not"
        );
        assert_eq!(client.profile(), session.profile);
    }

    /// A connection built from a spec knows which transport it dialled.
    ///
    /// The narrow assertion, on its own: [`connect`](Client::connect) takes a
    /// bare [`Negotiation`] where the caller supplies the four inputs by hand,
    /// and it is the one path that can record a transport the caller got
    /// wrong. Asserting it here means the two constructors are both covered
    /// for the field neither used to keep.
    #[test]
    fn a_client_built_by_hand_keeps_the_transport_it_was_given() {
        let version = ProtocolVersion::V2025_03_26;
        let caps = CapabilitySet::all_for(version);
        let client = Client::connect(
            crate::domain::Negotiation {
                version,
                client_caps: caps,
                server_caps: caps,
                transport: TransportKind::Stdio,
            },
            Arc::new(Recorder::dead()),
        );

        assert_eq!(client.session().transport, TransportKind::Stdio);
        assert_eq!(client.session().client_caps, caps);
        assert_eq!(
            client.session().server_info,
            None,
            "a bare Negotiation carries no peer names, and inventing them is worse than none"
        );
    }

    /// The error type for `connect_stdio` is a distinct `enum` from
    /// `ConnectError` because the spawn stage is stdio-specific. The
    /// variants' `Display` impls name the stage that failed, which is
    /// the one bit of information a caller retrying on transient
    /// transport faults needs. This test exists to keep the type
    /// reachable from the lib's own test target — the
    /// `dead_code_pub_in_binary` lint denies unused `pub` items, and
    /// the doctest that exercises the function does not name the
    /// error type directly (it uses `Box<dyn Error>`).
    #[cfg(feature = "stdio")]
    #[test]
    fn stdio_connect_error_names_the_stage() {
        use super::StdioConnectError;
        let err = StdioConnectError::Versions(crate::domain::SupportedVersionsError::Empty);
        let rendered = format!("{err}");
        assert!(
            rendered.contains("revision table"),
            "Versions variant must name the stage: {rendered}"
        );
    }
}
