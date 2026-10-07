//! Everything a Streamable HTTP server is told to do, in one typed value.
//!
//! # Why one value rather than constructor arguments
//!
//! Because the router, the session store and the SSE responder all need these
//! numbers, and three structs assembled separately is three ways for them to
//! disagree. The specific disagreement that matters: a replay capacity in the
//! store that is smaller than the window a client was promised would turn a
//! resumable stream into a silently truncated one, and no single component
//! would be able to see it. One `ServerConfig`, cloned into each, makes the
//! mismatch unrepresentable.
//!
//! # Why these are newtypes and not `String`/`usize` fields
//!
//! The project's rule is that a raw string never crosses an adapter boundary
//! once it has been parsed, and that holds here in both directions. An
//! [`Endpoint`] is validated when it is parsed rather than when it is mounted,
//! so an empty path or one without a leading `/` is a `Result` at the call
//! site instead of a panic — or a silent 404 — inside axum. [`MaxFrameSize`]
//! is the domain's own ceiling, reused rather than restated: the stdio
//! transport checks a frame against it and so does this one, and two limits
//! spelled separately is one of them eventually wrong.

use core::num::{NonZeroU32, NonZeroUsize};
use core::str::FromStr;
use core::time::Duration;
use std::sync::Arc;

use thiserror::Error;

use crate::domain::MaxFrameSize;
use crate::domain::RateLimit;
use crate::domain::mcp::ResourceMetadata;
use crate::domain::registry::ScopeSet;

/// The path a client POSTs, GETs and DELETEs, without a scheme or host.
///
/// The specification calls this the "MCP endpoint" and gives `https://
/// example.com/mcp` as its example. Only the path is kept, because the adapter
/// is handed a `Router` and mounting is the caller's decision — a server that
/// also serves a web UI mounts the same router under a different base and the
/// endpoint is the part the MCP specification actually constrains.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Endpoint(Arc<str>);

impl Endpoint {
    /// The default the specification's own example uses.
    pub const DEFAULT: &'static str = "/mcp";

    /// The path, for `axum::Router::route`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Endpoint {
    type Err = EndpointError;

    /// # Errors
    ///
    /// [`EndpointError::Empty`] for a blank path,
    /// [`EndpointError::NoLeadingSlash`] because axum matches paths that begin
    /// with one and would otherwise register a route that can never fire,
    /// [`EndpointError::QueryOrFragment`] because a `?` or `#` never reaches a
    /// server — the client strips both before writing the request line — so an
    /// endpoint containing one is a route that can never fire *and* a
    /// configuration mistake, and
    /// [`EndpointError::NotVisibleAscii`] because a path is spliced into a
    /// request line.
    ///
    /// Surrounding whitespace is trimmed rather than refused, on the same
    /// grounds as `McpSessionId::validate`: a path copied out of a config file
    /// or a URL bar routinely arrives with a trailing newline, and treating
    /// that as a hard error is a footgun with no security value.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(EndpointError::Empty);
        }
        if !trimmed.starts_with('/') {
            return Err(EndpointError::NoLeadingSlash);
        }
        if trimmed.contains(['?', '#']) {
            return Err(EndpointError::QueryOrFragment);
        }
        if !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b'/')
        {
            return Err(EndpointError::NotVisibleAscii);
        }
        Ok(Self(Arc::from(trimmed)))
    }
}

/// Why a path is not a usable endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EndpointError {
    /// The path was blank.
    #[error("the MCP endpoint path is empty")]
    Empty,
    /// The path did not start with `/`.
    ///
    /// Unit rather than carrying the offending path as a `&'static str`: the
    /// path came in as a borrowed `&str` with an arbitrary lifetime, so the
    /// only ways to carry it were to copy it into an owned allocation (for an
    /// error variant that fires once, at startup, on a value the caller still
    /// holds) or to name a lifetime on the error type. Neither is worth it —
    /// [`Endpoint`] is `Copy`-cheap to re-parse if the message is not specific
    /// enough, and the caller has the input.
    #[error("the MCP endpoint path must start with '/'")]
    NoLeadingSlash,
    /// The path carried a `?` or a `#`.
    ///
    /// Neither survives the trip from a URL to a server: the client splits the
    /// fragment off before the request and the query into a separate
    /// component, so an endpoint containing either names a path that can never
    /// be requested. Accepting it would register a route that silently never
    /// fires, which is the same failure as a typo and is harder to spot.
    #[error(
        "the MCP endpoint path must be a path only: a query string or fragment never reaches the server"
    )]
    QueryOrFragment,
    /// The path held a byte that cannot appear in a request line.
    #[error("the MCP endpoint path contains a character that is not valid in a request line")]
    NotVisibleAscii,
}

/// Whether a POST that warrants a reply is answered inline or over SSE.
///
/// The specification makes this the server's choice — a request "MUST either
/// return `Content-Type: text/event-stream` ... or `Content-Type:
/// application/json`" — and the two have genuinely different costs, which is
/// why it is configuration rather than a heuristic.
///
/// `Json` is the default because it is the cheaper of the two for the common
/// case: one request, one response, one round trip, and no connection held open
/// afterwards. `EventStream` costs a long-lived response for every request
/// whether or not the client ever needs a second message, and buys
/// server-initiated notifications on the same connection and a resumable
/// cursor. A server that wants those wants them for *every* request, because a
/// client cannot know in advance which request will turn out to need a
/// progress notification.///
/// # The router does not enforce this, and cannot
///
/// This is the one piece of [`ServerConfig`] the router does not read, and the
/// reason is structural rather than an oversight.
/// [`RequestHandler::dispatch`](super::RequestHandler::dispatch) returns a
/// fully framed [`axum::response::Response`], so by the time a `POST` reaches
/// the protocol layer its `Content-Type` is already decided. The router has
/// nothing left to apply the mode to.
///
/// The consequence is that a handler can frame its answer against the mode or
/// ignore it, and nothing here will notice — the same shape as T11's
/// `auth.rs` being reachable from no real path. The transport layer has no
/// remedy: making `dispatch` return a body and letting the router frame it
/// would move a transport concern into the protocol layer's signature, and the
/// protocol layer is the thing that has to stay protocol-neutral.
///
/// So it is recorded here and pinned by
/// `the_streaming_mode_never_changes_the_get_route` rather than enforced. T18
/// is where the framing is decided, and that is the code that has to honour
/// this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Streaming {
    /// Answer with one `application/json` document. The default.
    #[default]
    Json,
    /// Answer with a `text/event-stream` carrying the same document.
    EventStream,
}

/// The three response headers the project's security rules require on every
/// response.
///
/// Held as a value rather than written at each call site because the rule is
/// "every response", and the way that rule gets broken is a fourth route added
/// later that sets its own headers and forgets these. One value, attached by
/// one router-level layer, has no per-route path to miss.
///
/// The defaults are the strictest spelling of each header rather than a
/// permissive one. `default-src 'none'` denies the response every fetch
/// directive it does not explicitly re-grant, and an MCP endpoint serves no
/// scripts, styles, frames or connections at all, so there is nothing to
/// re-grant. `frame-ancestors 'none'` is the modern form and is why `X-Frame-
/// Options` is still sent alongside it: the older header is what a browser
/// predating CSP consults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityHeaders {
    content_security_policy: Arc<str>,
    strict_transport_security: Arc<str>,
    x_frame_options: Arc<str>,
}

impl Default for SecurityHeaders {
    fn default() -> Self {
        Self {
            content_security_policy: Arc::from(
                "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
            ),
            strict_transport_security: Arc::from("max-age=31536000; includeSubDomains"),
            x_frame_options: Arc::from("DENY"),
        }
    }
}

impl SecurityHeaders {
    /// Replaces the `Content-Security-Policy` value.
    #[must_use]
    pub fn with_content_security_policy(mut self, value: impl Into<Arc<str>>) -> Self {
        self.content_security_policy = value.into();
        self
    }

    /// Replaces the `Strict-Transport-Security` value.
    #[must_use]
    pub fn with_strict_transport_security(mut self, value: impl Into<Arc<str>>) -> Self {
        self.strict_transport_security = value.into();
        self
    }

    /// Replaces the `X-Frame-Options` value.
    #[must_use]
    pub fn with_x_frame_options(mut self, value: impl Into<Arc<str>>) -> Self {
        self.x_frame_options = value.into();
        self
    }

    /// The `Content-Security-Policy` value.
    #[must_use]
    pub fn content_security_policy(&self) -> &str {
        &self.content_security_policy
    }

    /// The `Strict-Transport-Security` value.
    #[must_use]
    pub fn strict_transport_security(&self) -> &str {
        &self.strict_transport_security
    }

    /// The `X-Frame-Options` value.
    #[must_use]
    pub fn x_frame_options(&self) -> &str {
        &self.x_frame_options
    }
}

/// How long a server-initiated SSE stream stays open before the server closes
/// it.
///
/// Not a nicety. The specification says the server "MAY close the SSE stream at
/// any time", and a server that never does holds one connection, one task and
/// one ring borrow per idle client for as long as the client is patient. The
/// default is long enough that a well-behaved client is never disconnected
/// mid-exchange and short enough that an abandoned connection is reclaimed.
///
/// A client that is dropped early reconnects with `Last-Event-ID` and loses
/// nothing, because the events are in the ring rather than in the connection.
/// That is the property that makes the timeout safe, and it is why the ring
/// exists separately from the stream.
const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_secs(30);

/// T17. Requests in flight at once before the connection is shed.
///
/// Not a queue length: past this the caller gets `503`. Sized for a
/// single-process server whose handlers are mostly I/O against one
/// authorization server — well above what a single client can generate, and
/// well below what exhausts the connection pool.
const DEFAULT_MAX_CONCURRENCY: NonZeroU32 = match NonZeroU32::new(256) {
    Some(value) => value,
    None => unreachable!(),
};

/// T17. The whole-request deadline.
///
/// Thirty seconds is far longer than a healthy tool call and far shorter than
/// the point at which a caller has given up, so a hung handler is dropped
/// while the client is still listening.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// T17. Requests per subject per minute.
///
/// Applied per `Mcp-Session-Id`, or shared across anonymous callers. The
/// window is a minute because that is the granularity an operator reasons
/// about when asked "who is hammering this"; the fixed window's worst case is
/// documented on [`RateLimit`].
const DEFAULT_RATE_LIMIT: RateLimit = RateLimit::new(
    match NonZeroU32::new(600) {
        Some(value) => value,
        None => unreachable!(),
    },
    DEFAULT_RATE_WINDOW,
);

/// The width of [`DEFAULT_RATE_LIMIT`].
const DEFAULT_RATE_WINDOW: Duration = Duration::from_secs(60);

/// How long a stream waits before emitting an SSE comment when it has nothing
/// to say.
///
/// A comment line (`:`) is a keep-alive that the SSE standard defines and every
/// client ignores. Without one, a proxy between here and the client is free to
/// decide an idle connection is dead — commonly at 60s — and would close it
/// while the server still believed the stream was healthy.
const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(15);

/// The settings one Streamable HTTP server is built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    endpoint: Endpoint,
    max_body: MaxFrameSize,
    replay_capacity: NonZeroUsize,
    streaming: Streaming,
    stream_timeout: Duration,
    keep_alive: Duration,
    security: SecurityHeaders,
    required_scopes: ScopeSet,
    resource_metadata: Option<Arc<ResourceMetadata>>,
    max_concurrency: NonZeroU32,
    request_timeout: Duration,
    rate_limit: RateLimit,
}

impl ServerConfig {
    /// A configuration with every default: the specification's endpoint, the
    /// domain's default frame ceiling, a replay window of
    /// [`RECOMMENDED_CAPACITY`](crate::domain::RECOMMENDED_CAPACITY), inline
    /// JSON answers, and the strict security headers.
    ///
    /// # Errors
    ///
    /// [`EndpointError`], because the default endpoint is parsed rather than
    /// trusted — a `const` that could be wrong is a bug the caller finds out
    /// about at construction instead of at mount time.
    pub fn new() -> Result<Self, EndpointError> {
        Ok(Self {
            endpoint: Endpoint::DEFAULT.parse()?,
            max_body: MaxFrameSize::DEFAULT,
            replay_capacity: crate::domain::RECOMMENDED_CAPACITY.into(),
            streaming: Streaming::default(),
            stream_timeout: DEFAULT_STREAM_TIMEOUT,
            keep_alive: DEFAULT_KEEP_ALIVE,
            security: SecurityHeaders::default(),
            required_scopes: ScopeSet::empty(),
            resource_metadata: None,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            rate_limit: DEFAULT_RATE_LIMIT,
        })
    }

    /// How many requests may be in flight at once.
    ///
    /// A ceiling rather than a queue: past this the connection is shed with
    /// `503`, because a request that waits for a slot has already spent the
    /// caller's timeout doing nothing. OWASP API Security Top 10:2023,
    /// API4:2023 (Unrestricted Resource Consumption).
    #[must_use]
    pub const fn max_concurrency(&self) -> NonZeroU32 {
        self.max_concurrency
    }

    /// Sets the in-flight ceiling. A `limit` of zero is rejected by
    /// [`NonZeroU32`], because a server that accepts nothing is a server that
    /// is up and useless.
    #[must_use]
    pub const fn with_max_concurrency(mut self, limit: NonZeroU32) -> Self {
        self.max_concurrency = limit;
        self
    }

    /// How long one request may take end to end.
    ///
    /// Distinct from [`stream_timeout`](Self::stream_timeout), which bounds
    /// the idle gap *between* events on an already-open stream. This one
    /// bounds the whole request, so a handler that never answers is dropped
    /// rather than holding a connection open forever.
    #[must_use]
    pub const fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Sets the per-request deadline.
    #[must_use]
    pub const fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// The per-subject request allowance.
    #[must_use]
    pub const fn rate_limit(&self) -> &RateLimit {
        &self.rate_limit
    }

    /// Sets the per-subject allowance.
    #[must_use]
    pub const fn with_rate_limit(mut self, limit: RateLimit) -> Self {
        self.rate_limit = limit;
        self
    }

    /// The MCP endpoint path.
    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The largest POST body accepted, in bytes.
    ///
    /// Enforced before the body is read into memory where the framework allows
    /// it, and again on the assembled buffer, because the two limits protect
    /// different things: the first the process, the second the framing layer.
    #[must_use]
    pub const fn max_body(&self) -> MaxFrameSize {
        self.max_body
    }

    /// How many events a session retains for replay.
    #[must_use]
    pub const fn replay_capacity(&self) -> NonZeroUsize {
        self.replay_capacity
    }

    /// Whether a POST is answered inline or over SSE.
    #[must_use]
    pub const fn streaming(&self) -> Streaming {
        self.streaming
    }

    /// Sets the streaming policy.
    #[must_use]
    pub const fn with_streaming(mut self, streaming: Streaming) -> Self {
        self.streaming = streaming;
        self
    }

    /// How long a server-initiated stream stays open.
    #[must_use]
    pub const fn stream_timeout(&self) -> Duration {
        self.stream_timeout
    }

    /// Sets the stream timeout. A zero timeout closes the stream as soon as
    /// its replay window is drained, which is a legitimate way to run a
    /// "deliver the backlog, then hang up" endpoint.
    #[must_use]
    pub const fn with_stream_timeout(mut self, timeout: Duration) -> Self {
        self.stream_timeout = timeout;
        self
    }

    /// How long a stream waits before emitting a keep-alive comment.
    #[must_use]
    pub const fn keep_alive(&self) -> Duration {
        self.keep_alive
    }

    /// Sets the keep-alive interval.
    #[must_use]
    pub const fn with_keep_alive(mut self, keep_alive: Duration) -> Self {
        self.keep_alive = keep_alive;
        self
    }

    /// The three security headers.
    #[must_use]
    pub const fn security(&self) -> &SecurityHeaders {
        &self.security
    }

    /// Replaces the security headers.
    #[must_use]
    pub fn with_security(mut self, security: SecurityHeaders) -> Self {
        self.security = security;
        self
    }

    /// The scopes a stateful request must hold.
    ///
    /// Empty by default, and empty is *not* the same as unauthenticated. An
    /// empty requirement is a statement that this endpoint has nothing to
    /// protect; a caller that disagrees names the scope it needs and every
    /// request that arrives without it is refused. The alternative — a
    /// default that demands a scope — would make the transport refuse every
    /// request from every deployment that has not yet wired T16's OAuth, which
    /// is a worse failure than serving a read-only endpoint to the public.
    ///
    /// What the empty default *does* guarantee is fail-secure structure rather
    /// than fail-secure policy: an anonymous caller is
    /// [`AuthContext::anonymous`](crate::domain::registry::AuthContext::anonymous),
    /// which holds no scopes, so the moment a scope is named the anonymous
    /// caller is refused. There is no path by which an unauthenticated request
    /// reaches a scope-bearing route.
    #[must_use]
    pub const fn required_scopes(&self) -> ScopeSet {
        self.required_scopes
    }

    /// Names the scopes a stateful request must hold.
    #[must_use]
    pub const fn with_required_scopes(mut self, scopes: ScopeSet) -> Self {
        self.required_scopes = scopes;
        self
    }

    /// The RFC 9728 metadata this server publishes, if it publishes one.
    ///
    /// Only used to build the `WWW-Authenticate` challenge a `401` carries, and
    /// `None` by default because publishing a document this crate does not
    /// serve would be a lie. A deployment that wants the challenge points this
    /// at the document its authorization server setup actually exposes.
    #[must_use]
    pub fn resource_metadata(&self) -> Option<&ResourceMetadata> {
        self.resource_metadata.as_deref()
    }

    /// Publishes `metadata`, enabling the `WWW-Authenticate` challenge.
    #[must_use]
    pub fn with_resource_metadata(mut self, metadata: ResourceMetadata) -> Self {
        self.resource_metadata = Some(Arc::new(metadata));
        self
    }

    /// The `WWW-Authenticate` challenge for a `401`, if this server publishes
    /// metadata.
    ///
    /// RFC 9728 §5.1: a `401` on a protected resource carries a
    /// `WWW-Authenticate` naming where the client can discover the
    /// authorization server, and that challenge is the only pointer a client
    /// gets. Without it a client holding an expired token has no URL to fetch
    /// authorization-server metadata from, which is the situation the challenge
    /// exists to solve.
    ///
    /// Returns `None` rather than a bare `Bearer` when nothing is published:
    /// a challenge naming no resource metadata tells the client the scheme and
    /// nothing else, and a deployment that has not configured a document is
    /// better served by an unadorned `401` that it can recognise as
    /// misconfigured.
    pub fn challenge(&self) -> Option<String> {
        self.resource_metadata.as_ref()?.challenge().ok()
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::{Endpoint, EndpointError, SecurityHeaders, ServerConfig, Streaming};

    type TestError = Box<dyn std::error::Error>;

    /// The default config parses, and its endpoint is the specification's own
    /// example rather than a bare `/`.
    #[test]
    fn the_default_endpoint_is_the_specification_example() -> Result<(), TestError> {
        let config = ServerConfig::new()?;
        assert_eq!(config.endpoint().as_str(), "/mcp");
        Ok(())
    }

    /// An endpoint is validated where it is parsed, not where it is mounted.
    ///
    /// Each of these would otherwise become a route that silently never fires
    /// — the failure mode of a typo in a configuration file, which is exactly
    /// the input this parse exists to catch.
    #[test]
    fn a_malformed_endpoint_is_refused_at_parse_time() -> Result<(), TestError> {
        for case in ["", "   ", "mcp", "mcp/stream", "/mcp stream", "/mcp\0mcp"] {
            assert!(
                case.parse::<Endpoint>().is_err(),
                "accepted {case:?} as an endpoint"
            );
        }
        // A query string and a fragment are the subtle pair, and the reason
        // `QueryOrFragment` is a variant of its own rather than falling into
        // the control-character arm: both bytes are perfectly printable ASCII,
        // so a naive "is it a legal request line character" check accepts them.
        // Neither survives the trip to a server, though — the client splits
        // the fragment off and the query into a separate component — so
        // `/mcp?debug=1` would register a route that can never be requested.
        assert_eq!(
            "/mcp?debug=1".parse::<Endpoint>(),
            Err(EndpointError::QueryOrFragment)
        );
        assert_eq!(
            "/mcp#frag".parse::<Endpoint>(),
            Err(EndpointError::QueryOrFragment)
        );
        assert_eq!(
            "mcp".parse::<Endpoint>(),
            Err(EndpointError::NoLeadingSlash)
        );
        assert_eq!("/a/b".parse::<Endpoint>()?.as_str(), "/a/b");
        // Surrounding whitespace is transport framing a caller may have copied
        // in, not an error; the same leniency `McpSessionId::validate` applies.
        // A trailing newline is whitespace like any other, which is why it is
        // asserted to be *accepted* here rather than refused: a config file
        // whose last line ends in a newline must not fail to start.
        assert_eq!("  /mcp  ".parse::<Endpoint>()?.as_str(), "/mcp");
        assert_eq!("/mcp\n".parse::<Endpoint>()?.as_str(), "/mcp");
        Ok(())
    }

    /// A well-formed endpoint survives the round trip, byte for byte.
    ///
    /// The parse trims and the parse is a lossy-looking operation, so the
    /// property worth pinning is that a path with no framing around it comes
    /// out exactly as it went in — a mount that silently dropped or doubled a
    /// segment would be a 404 nobody could explain.
    #[test]
    fn a_well_formed_endpoint_round_trips() -> Result<(), TestError> {
        for case in ["/mcp", "/", "/a/b", "/v1/mcp", "/mcp_v2", "/a-b_c.d~e"] {
            assert_eq!(case.parse::<Endpoint>()?.as_str(), case);
            // `Display` and `as_str` must agree, because one is what a mount
            // uses and the other is what an operator sees in a log.
            assert_eq!(case.parse::<Endpoint>()?.to_string(), case);
        }
        Ok(())
    }

    /// Every security header has a specific, enforced value by default.
    ///
    /// A header that is present but empty is worse than absent: a browser
    /// reads `X-Frame-Options: ` as no policy at all, and an empty CSP as no
    /// policy at all. So this asserts the *values*, not merely that the
    /// accessors return something — "the field exists" is exactly the test
    /// that passes against a default of `""`.
    #[test]
    fn the_security_headers_default_to_something_enforced() {
        let headers = SecurityHeaders::default();

        let csp = headers.content_security_policy();
        assert!(!csp.trim().is_empty(), "an empty CSP is no policy at all");
        // The endpoint serves no scripts, styles, frames or connections, so
        // `default-src 'none'` denies everything without re-granting anything.
        assert!(csp.contains("default-src 'none'"), "csp was {csp:?}");
        assert!(csp.contains("frame-ancestors 'none'"), "csp was {csp:?}");

        // HSTS is worthless without a max-age, and the includeSubDomains flag
        // is what stops a sibling host serving the same name over plain HTTP.
        let hsts = headers.strict_transport_security();
        assert!(hsts.contains("max-age="), "hsts was {hsts:?}");
        assert!(hsts.contains("includeSubDomains"), "hsts was {hsts:?}");

        // `DENY` is the strictest legal value. `SAMEORIGIN` would be a
        // weakening of the default and must have to be asked for.
        assert_eq!(headers.x_frame_options(), "DENY");
    }

    /// Each builder override reaches the value the layer will write, and only
    /// the one it names.
    ///
    /// Written as one test per header rather than a loop, because the failure
    /// this guards against is a setter that stores into the wrong field — and
    /// a loop reports that as "one of three failed", which is not a message
    /// anyone can act on at three in the morning.
    #[test]
    fn each_security_header_builder_takes_effect() {
        let csp = SecurityHeaders::default().with_content_security_policy("default-src 'self'");
        assert_eq!(csp.content_security_policy(), "default-src 'self'");
        // ...and the other two are untouched by the setter that did not name
        // them, which is the half of "takes effect" that is easy to get wrong.
        assert_eq!(csp.x_frame_options(), "DENY");
        assert!(csp.strict_transport_security().contains("max-age="));

        let hsts = SecurityHeaders::default().with_strict_transport_security("max-age=60");
        assert_eq!(hsts.strict_transport_security(), "max-age=60");
        assert_eq!(hsts.x_frame_options(), "DENY");

        let frame = SecurityHeaders::default().with_x_frame_options("SAMEORIGIN");
        assert_eq!(frame.x_frame_options(), "SAMEORIGIN");
        assert!(
            frame
                .content_security_policy()
                .contains("default-src 'none'")
        );

        // A builder must not leak back into the value it was copied from.
        let base = SecurityHeaders::default();
        let _widened = base.clone().with_x_frame_options("SAMEORIGIN");
        assert_eq!(base.x_frame_options(), "DENY");
    }

    /// The two setters reach the values the layer will write.
    #[test]
    fn the_config_carries_each_setting_it_accepts() -> Result<(), TestError> {
        let config = ServerConfig::new()?
            .with_streaming(Streaming::EventStream)
            .with_stream_timeout(Duration::from_secs(1))
            .with_keep_alive(Duration::from_millis(250))
            .with_security(SecurityHeaders::default().with_x_frame_options("SAMEORIGIN"));
        assert_eq!(config.streaming(), Streaming::EventStream);
        assert_eq!(config.stream_timeout(), Duration::from_secs(1));
        assert_eq!(config.keep_alive(), Duration::from_millis(250));
        assert_eq!(config.security().x_frame_options(), "SAMEORIGIN");
        // A builder must not leak into the value it was copied from: the
        // defaults are still the defaults on a freshly built config.
        assert_eq!(ServerConfig::new()?.streaming(), Streaming::Json);
        Ok(())
    }

    /// The replay window is a real capacity, and never zero.
    ///
    /// A zero-capacity ring accepts a push and drops it immediately, which is
    /// a livelock dressed as a buffer — so the field is a `NonZeroUsize` and
    /// this test is here to say that is deliberate rather than incidental.
    #[test]
    fn the_replay_capacity_cannot_be_zero() -> Result<(), TestError> {
        assert!(ServerConfig::new()?.replay_capacity().get() > 0);
        Ok(())
    }
}
