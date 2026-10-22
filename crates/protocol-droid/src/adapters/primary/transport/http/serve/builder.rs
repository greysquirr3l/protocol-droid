//! Assembling the axum `Router`.
//!
//! [`HttpBuilder`](super::HttpBuilder) collects the four decisions a socket
//! server needs — a session store, an id source, the handler, the ceiling —
//! and [`routes`] is the part that turns them into axum routes. It lives apart
//! from the handler because it is a different kind of code: everything here is
//! wiring, and everything in the parent module is a decision about a request.
//!
//! Two of the defaults here are `cfg`-gated on `random-ids`. A session id is
//! presented by the peer on every subsequent request, so a guessable sequence
//! is a guessable session; a lean build that cannot mint one gets an error at
//! startup rather than a counter.

use std::net::SocketAddr;
use std::sync::Arc;

use super::{HttpHandler, HttpSession, Ids};
use crate::adapters::primary::transport::http::config::ServerConfig;
use crate::adapters::primary::transport::http::limits::RateLimiter;
use crate::adapters::primary::transport::http::router::McpRouter;
use crate::adapters::primary::transport::http::session_store::SessionStore;
use crate::adapters::primary::transport::http::{AuthPolicy, Authenticator};
use crate::adapters::primary::transport::origin::OriginPolicy;
use crate::application::server::Server;
use crate::application::server::dispatch::Dispatcher;
use crate::domain::{ProtocolVersion, SupportedVersions};

/// The builder behind the server transport.
///
/// Every method is optional and every default is safe. The one decision this
/// builder does not make for the caller is which address to bind, and it
/// defaults that to loopback rather than to every interface.
pub struct HttpBuilder {
    /// The server whose tools and resources are served.
    server: Server,
    /// The address to listen on.
    addr: SocketAddr,
    /// The revision pre-handshake sessions are gated with.
    ceiling: ProtocolVersion,
    /// The endpoint policy and framing limits.
    ///
    /// Not defaulted here: [`ServerConfig::new`] parses the specification's
    /// endpoint and can fail, and a builder that cannot fail is worth more than
    /// one that resolves a constant early. An absent value is the crate's own
    /// defaults, resolved in [`serve`](Self::serve).
    config: Option<ServerConfig>,
    /// Where session ids come from.
    ids: Option<Ids>,
    /// The authenticator the router is built with.
    ///
    /// Absent means the router's default `Anonymous` authenticator; the
    /// `with_authenticator` setter records the choice so the route builder
    /// can pair it with the policy in the right order. See
    /// `McpRouter::with_authenticator` for the issue #11 fail-closed
    /// contract this is the consumer entry point for.
    authenticator: Option<Arc<dyn Authenticator + Send + Sync>>,
    /// The auth policy the router is built with.
    policy: Option<AuthPolicy>,
    /// The `Origin` allow-list the router enforces.
    origin_policy: Option<OriginPolicy>,
    /// The per-subject request allowance.
    rate_limiter: Option<RateLimiter>,
    /// Where the `2024-11-05` HTTP+SSE routes are mounted, when they are.
    #[cfg(feature = "legacy-sse")]
    legacy_sse: Option<LegacySsePaths>,
}

/// The two paths the legacy transport is mounted on.
#[cfg(feature = "legacy-sse")]
#[derive(Debug, Clone)]
struct LegacySsePaths {
    stream: String,
    message: String,
}

impl core::fmt::Debug for HttpBuilder {
    /// Prints what was configured and nothing else.
    ///
    /// Handwritten for the same reason as [`HttpHandler`]'s: a derived `Debug`
    /// would walk the [`Server`] and the id source, and neither belongs in a
    /// log line. The address and the ceiling are the two facts worth having
    /// when a serve has gone wrong.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("HttpBuilder")
            .field("addr", &self.addr)
            .field("ceiling", &self.ceiling)
            .field("config", &self.config)
            .field("ids_supplied", &self.ids.is_some())
            .finish_non_exhaustive()
    }
}

impl HttpBuilder {
    /// Serves `server`, on loopback unless told otherwise.
    ///
    /// The defaults are the crate's own strict ones rather than the permissive
    /// ones, so a builder that sets nothing is already safe: loopback, the
    /// newest handshake-capable revision, and a session id from the system
    /// entropy pool.
    #[must_use]
    pub fn new(server: Server) -> Self {
        Self {
            server,
            addr: SocketAddr::from(([127, 0, 0, 1], 3000)),
            ceiling: default_ceiling(),
            config: None,
            ids: None,
            authenticator: None,
            policy: None,
            origin_policy: None,
            rate_limiter: None,
            #[cfg(feature = "legacy-sse")]
            legacy_sse: None,
        }
    }

    /// Also serves the `2024-11-05` HTTP+SSE transport, beside Streamable HTTP.
    ///
    /// A client opens a stream with `GET /sse`, is told where to `POST` by the
    /// `endpoint` event, and speaks `2024-11-05` over the pair. The same
    /// tools, resources and prompts answer on both transports, and the
    /// authenticator, `Origin` policy and rate limit set on this builder apply
    /// to both. Use [`legacy_sse_at`](Self::legacy_sse_at) to move the paths.
    #[cfg(feature = "legacy-sse")]
    #[must_use]
    pub fn legacy_sse(self) -> Self {
        self.legacy_sse_at(
            crate::adapters::primary::transport::legacy_sse::DEFAULT_STREAM_PATH,
            crate::adapters::primary::transport::legacy_sse::DEFAULT_MESSAGE_PATH,
        )
    }

    /// [`legacy_sse`](Self::legacy_sse), on `stream` and `message` rather than
    /// `/sse` and `/messages`.
    ///
    /// Both must be absolute paths with no query; [`serve`](Self::serve)
    /// refuses to start otherwise.
    #[cfg(feature = "legacy-sse")]
    #[must_use]
    pub fn legacy_sse_at(mut self, stream: impl Into<String>, message: impl Into<String>) -> Self {
        self.legacy_sse = Some(LegacySsePaths {
            stream: stream.into(),
            message: message.into(),
        });
        self
    }

    /// The address to bind. Defaults to `127.0.0.1:3000`.
    ///
    /// A non-loopback address is permitted but not made safe: this server
    /// speaks plain HTTP, so whatever is in front of it has to terminate TLS
    /// for it to be exposed to a network rather than to a machine.
    #[must_use]
    pub const fn addr(mut self, addr: SocketAddr) -> Self {
        self.addr = addr;
        self
    }

    /// The revision to gate pre-handshake sessions with.
    #[must_use]
    pub const fn version(mut self, version: ProtocolVersion) -> Self {
        self.ceiling = version;
        self
    }

    /// The endpoint policy and framing limits.
    #[must_use]
    pub fn config(mut self, config: ServerConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Where session ids come from.
    ///
    /// Defaults to a counter from this crate's own source. Supply
    /// [`RandomIds`](crate::adapters::secondary::id_source::RandomIds) — behind
    /// the `random-ids` feature — for an id a peer cannot guess, which is the
    /// right choice anywhere a session id authorises anything.
    #[must_use]
    pub fn ids(mut self, ids: Ids) -> Self {
        self.ids = Some(ids);
        self
    }

    /// Replaces the anonymous default with a real implementation.
    ///
    /// Pair with [`with_policy`](Self::with_policy); a wired authenticator
    /// under the default `AuthPolicy::Public` is the silent fail-open the
    /// router's `into_router` refuses at mount time. The same contract
    /// applies here, surfaced one layer up: a deployment that sets this
    /// without `with_policy` will see the misconfiguration at
    /// [`serve`](Self::serve) rather than at the first request.
    #[must_use]
    pub fn with_authenticator(
        mut self,
        authenticator: Arc<dyn Authenticator + Send + Sync>,
    ) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// Replaces the default `AuthPolicy::Public` policy.
    ///
    /// A deployment that wired an authenticator above must also set
    /// `Require(scopes)` here, or `into_router` will refuse to build the
    /// route table. See `McpRouter::with_policy` for the matching
    /// contract.
    #[must_use]
    pub const fn with_policy(mut self, policy: AuthPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Replaces the default `Origin` allow-list.
    ///
    /// The default is `OriginPolicy::strict(None)`, which refuses every
    /// `Origin` header sent and allows a request that sent none — see
    /// `OriginPolicy::check` for why absent is not the same as foreign.
    /// A deployment serving a browser over a non-loopback address will
    /// need to widen this to the page's origin.
    #[must_use]
    pub fn with_origin_policy(mut self, origin: OriginPolicy) -> Self {
        self.origin_policy = Some(origin);
        self
    }

    /// Replaces the per-subject request allowance.
    ///
    /// Defaults to `ServerConfig::rate_limit()`, so a configuration that
    /// set one does not have to set it twice.
    #[must_use]
    pub fn with_rate_limiter(mut self, limiter: RateLimiter) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    /// Binds the listener without serving it.
    ///
    /// Split from serving because a caller terminating TLS itself needs the
    /// listener before anything is served on it, and because binding before
    /// accepting removes the window in which a connection is refused for a port
    /// this process is about to take.
    ///
    /// # Errors
    ///
    /// [`ServeError::Bind`] when the address is taken or unusable.
    pub async fn bind(self) -> Result<(tokio::net::TcpListener, ServeReport), ServeError> {
        let (listener, bound) = bind(self.addr).await?;
        let report = ServeReport {
            addr: bound,
            ceiling: self.ceiling,
        };
        Ok((listener, report))
    }

    /// Binds and serves until the accept loop fails.
    ///
    /// # Errors
    ///
    /// [`ServeError`] from building the route table, from binding, or from the
    /// accept loop itself. A protocol error is not one of those: a peer whose
    /// request this server cannot serve is answered on the wire rather than
    /// raised here.
    pub async fn serve(self) -> Result<ServeReport, ServeError> {
        let parts = routes(self)?;
        let (listener, bound) = bind(parts.addr).await?;
        axum::serve(listener, parts.router)
            .await
            .map_err(ServeError::Accept)?;
        Ok(ServeReport {
            addr: bound,
            ceiling: parts.ceiling,
        })
    }

    /// Serves on a listener the caller already bound.
    ///
    /// The other half of [`bind`](Self::bind), and the shape a deployment with
    /// TLS in front needs: bind, wrap the listener in a TLS acceptor, and serve
    /// on whatever that acceptor produces. Binding inside `serve` would make
    /// this impossible, because there would be no moment at which the raw
    /// listener was in the caller's hands.
    ///
    /// # Errors
    ///
    /// `ServeError::Routes` when the route table cannot be built, and
    /// [`ServeError::Accept`] when the loop does.
    pub async fn serve_on(
        self,
        listener: tokio::net::TcpListener,
    ) -> Result<ServeReport, ServeError> {
        let parts = routes(self)?;
        let bound = listener.local_addr().map_err(|source| ServeError::Bind {
            addr: parts.addr,
            source,
        })?;
        axum::serve(listener, parts.router)
            .await
            .map_err(ServeError::Accept)?;
        Ok(ServeReport {
            addr: bound,
            ceiling: parts.ceiling,
        })
    }
}

/// What `routes` returns: the assembled router and the two builder fields
/// the calling method still needs after the table is built.
#[derive(Debug)]
pub(super) struct BuilderOutput {
    /// The mounted `axum::Router`.
    pub(super) router: axum::Router,
    /// The address the caller is binding. Preserved for `ServeReport` and
    /// for the `Bind` error path.
    pub(super) addr: SocketAddr,
    /// The pre-handshake revision. Preserved for `ServeReport`.
    pub(super) ceiling: ProtocolVersion,
}

/// The route table this builder describes, and nothing that binds a socket.
///
/// Free so that both [`HttpBuilder::serve`] and [`HttpBuilder::serve_on`] build
/// the same table from the same parts. A second implementation here would be a
/// second answer to "what routes does this server have", and the two would
/// differ the first time either was edited.
pub(super) fn routes(builder: HttpBuilder) -> Result<BuilderOutput, ServeError> {
    let HttpBuilder {
        server,
        addr,
        ceiling,
        config,
        ids,
        authenticator,
        policy,
        origin_policy,
        rate_limiter,
        #[cfg(feature = "legacy-sse")]
        legacy_sse,
    } = builder;
    let ids = match ids {
        Some(ids) => ids,
        None => default_ids()?,
    };
    #[cfg(feature = "random-ids")]
    let server = {
        let mut server = server;
        server.ensure_request_state_key(crate::adapters::secondary::id_source::random_state_key);
        server
    };
    let config = match config {
        Some(config) => config,
        None => ServerConfig::new()?,
    };
    let dispatcher = Arc::new(Dispatcher::new(Arc::new(server)));
    let sessions = Arc::new(SessionStore::<HttpSession>::new(Arc::clone(&ids)));
    let handler = HttpHandler::new(Arc::clone(&dispatcher), Arc::clone(&sessions), ceiling);
    #[cfg(feature = "legacy-sse")]
    let legacy = legacy_sse
        .map(|paths| {
            let mut router = crate::adapters::primary::transport::legacy_sse::LegacyRouter::new(
                config.clone(),
                Arc::new(SessionStore::new(ids)),
                // The transport belongs to `2024-11-05`; a client offering a
                // later revision is answered with this one and decides for
                // itself whether to continue.
                crate::adapters::primary::transport::legacy_sse::ServerDispatcher::new(
                    Arc::clone(&dispatcher),
                    ProtocolVersion::V2024_11_05,
                ),
            )
            .with_stream_path(paths.stream)
            .with_message_path(paths.message);
            if let Some(origin) = origin_policy.clone() {
                router = router.with_origins(origin);
            }
            if let Some(limiter) = rate_limiter.clone() {
                router = router.with_rate_limiter(limiter);
            }
            if let (Some(policy), Some(authenticator)) = (policy, authenticator.clone()) {
                router = router.with_auth(policy, authenticator);
            }
            router.into_router()
        })
        .transpose()?;
    let mut router = McpRouter::new(config, sessions, handler);
    if let Some(authenticator) = authenticator {
        router = router.with_authenticator(authenticator);
    }
    if let Some(policy) = policy {
        router = router.with_policy(policy);
    }
    if let Some(origin) = origin_policy {
        router = router.with_origin_policy(origin);
    }
    if let Some(limiter) = rate_limiter {
        router = router.with_rate_limiter(limiter);
    }
    let router = router.into_router().map_err(ServeError::from)?;
    #[cfg(feature = "legacy-sse")]
    let router = match legacy {
        Some(legacy) => router.merge(legacy),
        None => router,
    };
    Ok(BuilderOutput {
        router,
        addr,
        ceiling,
    })
}

/// Test-only pass-through that builds a `BuilderOutput` from the four
/// arguments the original `routes` took, so the existing tests keep their
/// one-line call site while the production path goes through the
/// builder.
///
/// Not `#[cfg(test)]` because the tests live in this module and need
/// `pub(crate)`, but the function is documented and named so a future
/// reader does not confuse it with the production entry point.
#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "the function is test-only; `pub(crate)` is needed in test builds for the tests to reach it, but in lib builds nothing outside the crate can see it, and the lint does not have a per-cfg answer."
)]
pub(crate) fn test_routes(
    server: Server,
    ceiling: ProtocolVersion,
    config: Option<ServerConfig>,
    ids: Option<Ids>,
) -> Result<axum::Router, ServeError> {
    routes(HttpBuilder {
        server,
        addr: SocketAddr::from(([127, 0, 0, 1], 3000)),
        ceiling,
        config,
        ids,
        authenticator: None,
        policy: None,
        origin_policy: None,
        rate_limiter: None,
        #[cfg(feature = "legacy-sse")]
        legacy_sse: None,
    })
    .map(|parts| parts.router)
}

/// The id source a builder uses when none was supplied.
///
/// [`RandomIds`] behind the `random-ids` feature, and an error without it. A
/// counter would be the obvious fallback and is the wrong one: a session id is
/// presented by the peer on every subsequent request, so a guessable sequence
/// is a guessable session. Failing is the honest answer for a lean build, and
/// it fails once at startup rather than per request.
#[cfg(feature = "random-ids")]
fn default_ids() -> Result<Ids, ServeError> {
    use crate::adapters::secondary::id_source::RandomIds;
    RandomIds::new()
        .map(|ids| Arc::new(ids) as Ids)
        .map_err(|error| ServeError::IdSource {
            reason: error.to_string(),
        })
}

/// No id source is available in this build, and none was supplied.
#[cfg(not(feature = "random-ids"))]
fn default_ids() -> Result<Ids, ServeError> {
    Err(ServeError::NoIdSource)
}

/// The newest revision this build can complete a handshake on.
///
/// The same fallback as the stdio builder's, and for the same reason: an empty
/// supported set leaves the ceiling a question rather than a wrong answer. Not
/// `latest_known()`, which is `2026-07-28` — the revision SEP-2575 removed
/// `initialize` from, and a server gated on it could never finish talking to
/// anybody.
pub(super) fn default_ceiling() -> ProtocolVersion {
    SupportedVersions::all().map_or_else(
        |_| ProtocolVersion::latest_known(),
        |supported| supported.latest_handshake(),
    )
}

/// What [`HttpBuilder::bind`] reports.
///
/// The address the OS chose rather than the one that was asked for, because
/// `127.0.0.1:0` is the ordinary way to ask for a free port and the address a
/// caller needs in order to connect is the one that was bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeReport {
    /// The address actually bound.
    pub addr: SocketAddr,
    /// The revision ceiling pre-handshake sessions were gated with.
    ///
    /// The *ceiling*, not a negotiated revision: no peer has connected yet.
    pub ceiling: ProtocolVersion,
}

/// Why an HTTP server could not be brought up.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The address could not be listened on.
    #[error("could not bind {addr}: {source}")]
    Bind {
        /// What was asked for.
        addr: SocketAddr,
        /// What the OS said.
        #[source]
        source: std::io::Error,
    },

    /// The endpoint policy is not usable.
    #[error("the endpoint policy is not usable: {0}")]
    Endpoint(#[from] crate::adapters::primary::transport::http::config::EndpointError),

    /// The accept loop failed.
    #[error("the accept loop failed: {0}")]
    Accept(#[source] std::io::Error),

    /// The route table could not be built.
    ///
    /// In practice a configured security header that is not a legal HTTP
    /// header value, or the issue #11 fail-open combination of a wired
    /// authenticator and the default `AuthPolicy::Public` policy. Failing
    /// here rather than at the first request is the point: the alternative
    /// is a server that starts and then answers every request with a
    /// permission boundary that does not exist.
    #[error("the route table could not be built: {0}")]
    Routes(#[from] crate::adapters::primary::transport::http::router::RouterError),

    /// The legacy HTTP+SSE routes could not be mounted.
    ///
    /// A path that is not an absolute path on this origin, or a configured
    /// security header that is not a legal value.
    #[cfg(feature = "legacy-sse")]
    #[error("the legacy HTTP+SSE routes could not be mounted: {0}")]
    LegacyRoutes(#[from] crate::adapters::primary::transport::legacy_sse::RouteError),

    /// No session-id source is available in this build.
    #[error(
        "this build has no id source: build with `random-ids`, or pass one to \
         HttpBuilder::ids"
    )]
    NoIdSource,

    /// The system entropy pool could not be read.
    #[error("session ids could not be drawn: {reason}")]
    IdSource {
        /// What the entropy source said.
        reason: String,
    },
}

/// Listens on `addr`, and reports what the OS actually gave.
///
/// Free rather than a method because [`HttpBuilder::serve`] has already moved
/// its own fields out by the time it calls this, and re-reading them off a
/// partially-consumed builder is how the two copies drift apart.
async fn bind(addr: SocketAddr) -> Result<(tokio::net::TcpListener, SocketAddr), ServeError> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Bind { addr, source })?;
    let bound = listener
        .local_addr()
        .map_err(|source| ServeError::Bind { addr, source })?;
    Ok((listener, bound))
}
