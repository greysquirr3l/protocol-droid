//! The Streamable HTTP client: POST, GET, DELETE, and a resume that does not
//! lose or repeat an event.
//!
//! Four things this client knows, and nothing else. Which endpoint to talk to.
//! Which session the peer issued, captured from a response header and sent on
//! every request after. Whether a response is one JSON document or a stream.
//! And, when a stream breaks, which event the peer last actually *delivered* —
//! because that number, not the number of bytes read, is what a resume asks
//! for.
//!
//! It names no method, no revision, and no capability. What a request *means*
//! belongs to the layer above; this one moves bytes and keeps the session
//! handle. A client that grew a method table would be unusable for the bridge,
//! which has to carry revisions it does not understand.
//!
//! Two response shapes, and the choice between them is the peer's, made in the
//! `Content-Type` and nowhere else. A client that guessed — always streaming,
//! or always buffering — hangs on one of the two legal answers, and the hang is
//! indistinguishable from a peer that has stopped talking.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::str::FromStr;
use core::sync::atomic::{AtomicI64, Ordering};
use serde_json::{Map, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::adapters::primary::transport::http::{SseError, SseEvent};
use crate::application::bridge::Unsolicited;
use crate::application::client::{ClientTransport, TransportError};
use crate::application::jsonrpc::document::{
    notification_document, reply_result, request_document,
};
use crate::domain::ir::{JsonObject, MethodName};
use crate::domain::{EventId, McpSessionId, RequestId};

use super::error::{HttpClientError, preview};
use super::frames::{FrameError, FrameReader};

/// Maps a frame failure onto the one enum a caller above this layer matches.
///
/// `BlockTooLong` is the one variant that does not come straight from the
/// shared grammar, so it says what actually happened rather than borrowing a
/// codec error that would name the wrong thing.
impl From<FrameError> for HttpClientError {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::BlockTooLong { .. } => Self::StreamEnded { delivered: 0 },
            FrameError::Grammar { source } => Self::Frame { source },
        }
    }
}

/// The session header, as the peer spells it.
const SESSION_HEADER: &str = "mcp-session-id";
/// The resume header, as the peer spells it.
const LAST_EVENT_ID: &str = "last-event-id";
/// The `POST` verb, named once so an error can say what was in flight.
const POST: &str = "POST";
/// The `GET` verb, likewise.
const GET: &str = "GET";
/// The `DELETE` verb, likewise.
const DELETE: &str = "DELETE";

/// What a client accepts, because the peer may answer either way.
///
/// A client that sends only `application/json` never sees a stream, and one
/// that sends only `text/event-stream` cannot get a single response without
/// opening a stream to read one line. Both are legal and both are worse than
/// asking for what the client can actually read.
const ACCEPT: &str = "application/json, text/event-stream";
/// The media type that means "one JSON document, stream now closed".
const JSON: &str = "application/json";
/// The media type that means "an open event stream".
const EVENT_STREAM: &str = "text/event-stream";
/// The largest `Last-Event-ID` this client will put in a header.
///
/// A bound on our own rendering rather than on the peer's, so a future id type
/// that renders longer fails here — where the error can name the cursor — and
/// not at send time, where it would name the request.
const MAX_CURSOR: usize = 32;

/// How a client is told to behave.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use protocol_droid::adapters::secondary::transport::http::{
///     HttpClient, HttpClientConfig, Resumable, RetryBudget,
/// };
///
/// let config = HttpClientConfig::new("https://example.invalid/mcp")?
///     .with_request_timeout(Duration::from_secs(30));
/// // Building a client opens no socket; the first call does.
/// let _client = HttpClient::new(config)?;
/// let _policy = Resumable {
///     budget: RetryBudget::new(3),
/// };
/// # Ok::<(), protocol_droid::adapters::secondary::transport::http::HttpClientError>(())
/// ```
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    endpoint: String,
    request_timeout: Option<Duration>,
}

impl HttpClientConfig {
    /// A configuration pointed at `endpoint`.
    ///
    /// # Errors
    ///
    /// [`HttpClientError::Url`] if `endpoint` is not a URL a request can be
    /// built against. Checked here rather than at the first call, so a typo is
    /// a construction error and not a surprise on the wire.
    pub fn new(endpoint: &str) -> Result<Self, HttpClientError> {
        // Parsed for the check and then dropped: `reqwest` re-parses on every
        // request, and holding a second representation of the same string is
        // one more thing that can disagree with the first.
        reqwest::Url::parse(endpoint).map_err(|error| HttpClientError::Url {
            got: endpoint.to_owned(),
            reason: error.to_string(),
        })?;
        Ok(Self {
            endpoint: endpoint.to_owned(),
            request_timeout: None,
        })
    }

    /// Bounds how long one request may take, reading its body included.
    #[must_use]
    pub const fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    /// Removes the request timeout, so a stream may stay open indefinitely.
    ///
    /// The default is no timeout, because a streamable event stream is
    /// long-lived by definition and a timeout on one is a reconnect loop.
    #[must_use]
    pub const fn without_request_timeout(mut self) -> Self {
        self.request_timeout = None;
        self
    }

    /// The endpoint this configuration points at.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The request timeout, if one is set.
    #[must_use]
    pub const fn request_timeout(&self) -> Option<Duration> {
        self.request_timeout
    }
}

/// How many times a resumable read will reconnect before reporting failure.
///
/// A plain integer, and plain on purpose: the temptation to make this clever is
/// exactly how a resume becomes a loop. Every reconnect has to be a bounded
/// number of attempts, and the bound has to be visible at the call site rather
/// than buried in a backoff policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBudget {
    attempts: u32,
}

impl RetryBudget {
    /// A budget of `attempts` reconnects.
    #[must_use]
    pub const fn new(attempts: u32) -> Self {
        Self { attempts }
    }

    /// How many reconnects this budget allows.
    #[must_use]
    pub const fn attempts(self) -> u32 {
        self.attempts
    }
}

impl Default for RetryBudget {
    /// Three: enough to ride out a redeploy, few enough that a peer which is
    /// genuinely gone is reported rather than retried until a human notices.
    fn default() -> Self {
        Self::new(3)
    }
}

/// How a resumable read behaves when its stream drops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Resumable {
    /// How many reconnects are allowed.
    pub budget: RetryBudget,
}

/// A client for one endpoint.
///
/// Holds the session the peer issued, so the type is `Send + Sync` and one
/// client can drive concurrent requests. That is the point: the session id is
/// the shared state, and it is a header rather than a connection, so two
/// in-flight requests sharing it is normal and not a race.
#[derive(Debug)]
pub struct HttpClient {
    inner: reqwest::Client,
    config: HttpClientConfig,
    session: Mutex<Option<McpSessionId>>,
    /// The id to put on the next request.
    ///
    /// A counter and not a random value, for the reason `StdioClient` uses
    /// one: the domain does not call a generator, so a client's ids are as
    /// reproducible as its configuration, and two requests in flight on one
    /// client can never collide.
    next_id: AtomicI64,
    /// Frames a server volunteered on a response body this client was reading
    /// for something else.
    ///
    /// A server may answer one `POST` with a stream carrying the reply *and*
    /// any number of messages it wanted to send anyway. Those are real
    /// traffic and they are not this request's answer, so they are held here
    /// and handed to the bridge rather than dropped.
    pub(crate) unsolicited: Arc<Unsolicited>,
}

impl HttpClient {
    /// Builds a client. Opens no socket.
    ///
    /// # Errors
    ///
    /// Propagates the builder's own failure, which is a TLS backend that could
    /// not be initialised rather than anything about the endpoint.
    pub fn new(config: HttpClientConfig) -> Result<Self, HttpClientError> {
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("protocol-droid/", env!("CARGO_PKG_VERSION")));
        // `timeout` takes a `Duration`, not an `Option`, so an absent timeout
        // is the builder's own default of none rather than a value to undo.
        if let Some(limit) = config.request_timeout {
            builder = builder.timeout(limit);
        }
        let inner = builder.build().map_err(|error| HttpClientError::Io {
            method: "CLIENT",
            error,
        })?;
        Ok(Self::from_client(inner, config))
    }

    /// Builds a client around a connection pool that already exists.
    ///
    /// Exists for the auto-detect step, which holds a `reqwest::Client` before
    /// it knows which transport the peer speaks — the probe request is the thing
    /// that tells it. Handing that pool to whichever client wins means one TLS
    /// handshake and one DNS resolution for the whole connection, rather than a
    /// second set for the transport that happened to win.
    ///
    /// Takes the pool by value and cannot fail, because there is nothing left
    /// to validate: the configuration was checked by whoever parsed it, and the
    /// pool is already built.
    #[must_use]
    pub fn from_client(inner: reqwest::Client, config: HttpClientConfig) -> Self {
        Self {
            inner,
            config,
            session: Mutex::new(None),
            next_id: AtomicI64::new(1),
            unsolicited: Arc::new(Unsolicited::new()),
        }
    }

    /// The id for the next request on this client.
    pub fn next_request_id(&self) -> RequestId {
        RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// A client that already knows the session the peer issued.
    ///
    /// For the same probe: its response already carried an `Mcp-Session-Id`, and
    /// every later request has to carry it. Reconstructing the client from
    /// scratch to recover that header would mean re-running the handshake, and
    /// an `initialize` run twice is a second session on a server that is
    /// counting them.
    #[must_use]
    pub fn resuming(self, id: McpSessionId) -> Self {
        *self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id);
        self
    }

    /// The configuration this client was built with.
    #[must_use]
    pub const fn config(&self) -> &HttpClientConfig {
        &self.config
    }

    /// The session the peer has issued, if it has issued one.
    #[must_use]
    pub fn session_id(&self) -> Option<McpSessionId> {
        // Poisoning is not a reason to lose the session id. The value is a
        // header, no invariant a panic could have broken, and propagating
        // poisoning would turn one panicking thread into a client that can never
        // talk to the peer again.
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .copied()
    }

    /// POSTs a body and reads one complete response.
    ///
    /// Reads the body in full, so a response the peer chose to frame as a
    /// stream is **not** acceptable here: its body is open-ended and buffering
    /// it waits forever. Use [`HttpClient::post_streaming`] for that shape.
    ///
    /// # Errors
    ///
    /// Any [`HttpClientError`] a request can produce, including
    /// [`HttpClientError::NoSessionYet`] when `require_session` is set and the
    /// peer has not issued a session, and [`HttpClientError::MediaType`] when a
    /// `2xx` came back framed as a stream.
    pub async fn post(
        &self,
        body: &[u8],
        require_session: bool,
    ) -> Result<HttpResponse, HttpClientError> {
        let response = self.send(POST, body, require_session).await?;
        self.read_document(response, POST).await
    }

    /// POSTs a whole JSON-RPC envelope and returns the peer's answer envelope.
    ///
    /// The envelope-granularity entry point, for a caller that already holds a
    /// parsed message — a bridge, chiefly. [`ClientTransport::send`] cannot
    /// serve that, and the difference is not cosmetic:
    ///
    /// - `send` *mints* an id from this client's counter. A proxy must carry
    ///   the downstream id through instead, or the reply cannot be matched to
    ///   the request the client is still waiting on.
    /// - `send` returns `reply_result`, which pulls the `result` member out and
    ///   turns an `error` member into a [`TransportError`]. A proxy needs the
    ///   whole reply: a peer that declines a request has sent a well-formed
    ///   message, and re-wrapping it as a transport fault reports a network
    ///   error for a request the server answered on purpose.
    ///
    /// The session lifecycle is this client's, not a second one — the
    /// `Mcp-Session-Id` this client obtained at handshake is the one every
    /// forwarded request carries, which is what lets a bridge reuse an
    /// established upstream session instead of re-handshaking per request.
    ///
    /// `require_session` is `false` because a bridge forwards the handshake
    /// itself, and the first message it forwards is the one that has no session
    /// yet.
    ///
    /// # Errors
    ///
    /// Any [`HttpClientError`] a request can produce, plus
    /// [`HttpClientError::NotAMessage`] when the peer answers `2xx` with a JSON
    /// body that is not an object.
    ///
    /// # Panics
    ///
    /// Never. Serialisation and parsing failures are [`HttpClientError`]s.
    pub async fn post_envelope(
        &self,
        envelope: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, HttpClientError> {
        let body = serde_json::to_vec(envelope).map_err(|error| HttpClientError::Body {
            method: "POST",
            what: "encoded",
            detail: error.to_string(),
        })?;
        // `send` rather than `post`, because `post` insists on a whole
        // document. The specification lets a server answer a `POST` with
        // either a JSON document or an event stream, and insisting on the
        // first here failed every request against a server that chose the
        // second — which is the choice the specification prefers.
        let response = self.send(POST, &body, false).await?;
        let wants = envelope.get("id").cloned();
        if Self::shape(&response)? == Shape::Stream {
            return self.read_streamed(response, wants.as_ref()).await.map(Some);
        }
        let raw = response
            .bytes()
            .await
            .map_err(|error| HttpClientError::Io {
                method: POST,
                error,
            })?;
        let raw: &[u8] = &raw;

        // A notification carries no id, so the peer has nothing to match
        // against and the body is "received" regardless of shape. The
        // specification recommends `202` with no body, but a server that
        // answers with `200` and `null` is acknowledging the same thing, and
        // treating it as a transport fault makes the closing
        // `notifications/initialized` of every handshake fail. `None` is
        // the honest reading for a notification whether the body is empty,
        // `null`, or anything else that is not a JSON-RPC reply.
        if wants.is_none() {
            return Ok(None);
        }

        if raw.is_empty() {
            return Ok(None);
        }

        let text = std::str::from_utf8(raw).map_err(|error| HttpClientError::Body {
            method: "POST",
            what: "decoded as UTF-8",
            detail: error.to_string(),
        })?;
        let value: Value = serde_json::from_str(text).map_err(|error| HttpClientError::Body {
            method: "POST",
            what: "parsed as JSON",
            detail: error.to_string(),
        })?;

        value
            .as_object()
            .cloned()
            .map(Some)
            .ok_or_else(|| HttpClientError::NotAMessage {
                got: value.to_string(),
            })
    }

    /// Sends an already-assembled notification envelope.
    ///
    /// The envelope-granularity counterpart to
    /// [`ClientTransport::notify`], for a caller that already holds a parsed
    /// message. A notification is answered with a `202` and no body, so there
    /// is nothing to read back — this is [`HttpClient::post_envelope`] with
    /// the reply discarded, which is why it is a method rather than a call the
    /// adapter has to remember to write correctly itself.
    ///
    /// # Errors
    ///
    /// [`HttpClientError`] when the envelope cannot be encoded or the `POST`
    /// does not succeed.
    pub async fn notify_envelope(
        &self,
        envelope: &Map<String, Value>,
    ) -> Result<(), HttpClientError> {
        let body = serde_json::to_vec(envelope).map_err(|error| HttpClientError::Body {
            method: "POST",
            what: "encoded",
            detail: error.to_string(),
        })?;
        self.post(&body, false).await?;
        Ok(())
    }

    /// POSTs a body and accepts either response shape.
    ///
    /// # Errors
    ///
    /// As [`HttpClient::post`], except that a stream is a success rather than a
    /// [`HttpClientError::MediaType`].
    pub async fn post_streaming(
        &self,
        body: &[u8],
        require_session: bool,
    ) -> Result<HttpReply, HttpClientError> {
        let response = self.send(POST, body, require_session).await?;
        match Self::shape(&response)? {
            Shape::Json => self
                .read_document(response, POST)
                .await
                .map(HttpReply::Document),
            Shape::Stream => Ok(HttpReply::Stream(HttpStream::new(response))),
        }
    }

    /// DELETEs the session, ending it at the peer.
    ///
    /// # Errors
    ///
    /// [`HttpClientError::NoSessionYet`] when there is no session to end. There
    /// is nothing here to be idempotent about, so guessing is not on the menu.
    pub async fn delete(&self) -> Result<HttpResponse, HttpClientError> {
        let Some(id) = self.session_id() else {
            return Err(HttpClientError::NoSessionYet);
        };
        let response = self
            .inner
            .delete(&self.config.endpoint)
            .header(SESSION_HEADER, id.to_string())
            .header(reqwest::header::ACCEPT, ACCEPT)
            .send()
            .await
            .map_err(|error| HttpClientError::Io {
                method: DELETE,
                error,
            })?;
        self.read_document(response, DELETE).await
    }

    /// Opens an event stream from `cursor`, and does not reconnect.
    ///
    /// For a stream that has to survive a drop use [`HttpClient::resumable`],
    /// which is the same read with the reconnect inside it.
    ///
    /// # Errors
    ///
    /// [`HttpClientError::NoSessionYet`] when `require_session` is set and no
    /// session has been issued, or [`HttpClientError::MediaType`] if the peer
    /// did not answer with a stream.
    pub async fn open_stream(
        &self,
        cursor: Option<EventId>,
        require_session: bool,
    ) -> Result<HttpStream, HttpClientError> {
        let response = self.get(cursor, require_session).await?;
        if Self::shape(&response)? != Shape::Stream {
            return Err(media_error(&response));
        }
        Ok(HttpStream::new(response))
    }

    /// A read that reconnects itself when the peer's stream breaks.
    ///
    /// The three things this gets right, each of which is a way a resume
    /// silently loses or repeats an event:
    ///
    /// - It resumes from the last event **delivered**, never from a byte count
    ///   and never from the last block *read*. A chunk can carry half an
    ///   event; resuming from where reading stopped repeats it.
    /// - An event with **no id** cannot be resumed from, because the peer holds
    ///   no cursor for it. Resuming from the previous id would make the peer
    ///   repeat it, so the break is reported instead of papered over.
    /// - The first failure is not instantly fatal and neither is the hundredth:
    ///   there is a budget, and running out of it is an error carrying how far
    ///   the read got.
    ///
    /// Nothing is opened until the first [`ResumableStream::next_event`], so
    /// building one cannot fail and cannot hang.
    #[must_use]
    pub const fn resumable(
        &self,
        start: Option<EventId>,
        require_session: bool,
        policy: Resumable,
    ) -> ResumableStream<'_> {
        ResumableStream {
            client: self,
            current: None,
            cursor: start,
            delivered: 0,
            attempts: 0,
            require_session,
            policy,
            finished: false,
        }
    }

    /// Sends one request, absorbing the session header off the response.
    async fn send(
        &self,
        verb: &'static str,
        body: &[u8],
        require_session: bool,
    ) -> Result<reqwest::Response, HttpClientError> {
        let mut builder = self
            .inner
            .request(method_of(verb), &self.config.endpoint)
            .header(reqwest::header::ACCEPT, ACCEPT)
            .body(body.to_vec());
        if let Some(id) = self.session_id() {
            builder = builder.header(SESSION_HEADER, id.to_string());
        } else if require_session {
            return Err(HttpClientError::NoSessionYet);
        }
        let response = builder.send().await.map_err(|error| HttpClientError::Io {
            method: verb,
            error,
        })?;
        self.absorb_session(&response)?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(refusal(verb, response).await)
        }
    }

    /// The `GET` half of [`HttpClient::send`], which also carries the cursor.
    async fn get(
        &self,
        cursor: Option<EventId>,
        require_session: bool,
    ) -> Result<reqwest::Response, HttpClientError> {
        let mut builder = self
            .inner
            .get(&self.config.endpoint)
            .header(reqwest::header::ACCEPT, ACCEPT);
        if let Some(id) = self.session_id() {
            builder = builder.header(SESSION_HEADER, id.to_string());
        } else if require_session {
            return Err(HttpClientError::NoSessionYet);
        }
        if let Some(id) = cursor {
            builder = builder.header(LAST_EVENT_ID, cursor_value(id)?);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| HttpClientError::Io { method: GET, error })?;
        self.absorb_session(&response)?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(refusal(GET, response).await)
        }
    }

    /// Reads a `Mcp-Session-Id` off a response and remembers it.
    ///
    /// An unparseable id is an error rather than something to drop. A client
    /// that ignores it sends its next request with no session header, and the
    /// failure that follows points at the session layer instead of at the
    /// header that was actually wrong.
    fn absorb_session(&self, response: &reqwest::Response) -> Result<(), HttpClientError> {
        let Some(raw) = response.headers().get(SESSION_HEADER) else {
            return Ok(());
        };
        // Lossy on purpose: the replacement character is not a hex digit, so a
        // header that is not even text fails `from_str` with the same typed
        // error a well-formed-but-wrong header does. One way for this to go
        // wrong, rather than two.
        let text = String::from_utf8_lossy(raw.as_bytes());
        let id = McpSessionId::from_str(&text)
            .map_err(|source| HttpClientError::SessionId { source })?;
        *self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id);
        Ok(())
    }

    /// Decides which of the two readable shapes a `2xx` arrived in.
    fn shape(response: &reqwest::Response) -> Result<Shape, HttpClientError> {
        let found = media_type(response);
        if is_event_stream(found.as_deref()) {
            return Ok(Shape::Stream);
        }
        if is_json(found.as_deref()) {
            return Ok(Shape::Json);
        }
        Err(media_error(response))
    }

    /// Turns a `2xx` into a fully-read document.
    /// Reads a streamed answer, holding everything that was not the reply.
    ///
    /// The stream is a sequence of messages and the request has one id, so
    /// this reads until it sees that id and holds the rest. A frame that is
    /// not a JSON-RPC object at all is dropped rather than held: there is no
    /// shape downstream could forward it in.
    async fn read_streamed(
        &self,
        response: reqwest::Response,
        wants: Option<&Value>,
    ) -> Result<Map<String, Value>, HttpClientError> {
        let mut stream = HttpStream::new(response);
        while let Some(event) = stream.next_event().await? {
            let Ok(Value::Object(candidate)) = serde_json::from_str(event.data()) else {
                continue;
            };
            if candidate.get("id") == wants {
                return Ok(candidate);
            }
            self.unsolicited.hold(candidate);
        }
        // No answer owed: this was a notification, so the peer said `202` and
        // nothing is owed back. Anything it volunteered on the way is held
        // above. Reaching here means it closed the stream without a frame at
        // all, which is not a fault in a notification the caller does not read.
        if wants.is_none() {
            return Ok(Map::new());
        }
        // The stream closed without the answer. A peer that stops mid-answer
        // is a transport fault, and reporting it as one keeps a bridge from
        // forwarding a request it can never complete.
        Err(HttpClientError::Unanswered { method: POST })
    }

    /// Opens the session's standing stream and reads it until the peer ends it.
    ///
    /// ## Why a client that only posts needs this
    ///
    /// A Streamable HTTP server may push at any time — `list_changed`, a
    /// sampling request, a log message — on the stream a `GET` opens. A client
    /// that only ever POSTs never opens one, so those messages have nowhere to
    /// arrive, and a bridge forwards every request while telling its own client
    /// nothing. Opening the stream is what makes [`Self::take_pending`] mean
    /// anything.
    ///
    /// The read runs in its own task and parks into the same queue a mid-call
    /// frame lands in, so a caller cannot tell which of the two produced what,
    /// and one wait covers both.
    ///
    /// # Errors
    ///
    /// [`HttpClientError::NoSessionYet`] before a session exists — there is no
    /// stream to open without one — and any error a `GET` can produce. Failing
    /// to open is reported rather than swallowed.
    pub async fn listen(&self) -> Result<(), HttpClientError> {
        let mut stream = self.open_stream(None, true).await?;
        let parked = Arc::clone(&self.unsolicited);
        tokio::spawn(async move {
            // Ends when the peer closes the stream. No reconnect here:
            // `resumable` is the read that survives a drop, and re-establishing
            // a stream is a different decision with its own rules.
            while let Ok(Some(event)) = stream.next_event().await {
                // A frame that is not an object is not a JSON-RPC message and
                // has no shape to forward it in.
                if let Ok(Value::Object(frame)) = serde_json::from_str(event.data()) {
                    parked.hold(frame);
                }
            }
        });
        Ok(())
    }

    /// Takes every frame a server volunteered while this client was busy.
    #[must_use]
    pub fn take_pending(&self) -> Vec<Map<String, Value>> {
        self.unsolicited.take()
    }

    async fn read_document(
        &self,
        response: reqwest::Response,
        method: &'static str,
    ) -> Result<HttpResponse, HttpClientError> {
        let shape = Self::shape(&response)?;
        if shape == Shape::Stream {
            // Refused here, and only here. `read_document` is the caller that
            // wanted a whole JSON document and would otherwise hand a caller
            // SSE bytes under a JSON contract. Every other caller that can meet
            // a stream reads it as one; routing a call through here instead is
            // how a server that prefers `text/event-stream` — the preference
            // the specification states — ends up failing a handshake on its
            // closing notification.
            return Err(media_error(&response));
        }
        let content_type = media_type(&response);
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| HttpClientError::Io { method, error })?;
        Ok(HttpResponse {
            status,
            content_type,
            body: body.to_vec(),
            session: self.session_id(),
        })
    }
}

/// The two readable response shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// One JSON document; the body is now closed.
    Json,
    /// An open event stream.
    Stream,
}

/// An open event stream, read one whole frame at a time.
pub struct HttpStream {
    response: reqwest::Response,
    reader: FrameReader,
    /// Frames already assembled out of a previous read, waiting their turn.
    ///
    /// A read is not a frame. One TCP segment routinely carries several
    /// complete events, and taking only the first would silently drop the rest
    /// — a stream that is short by exactly the number of events a fast peer
    /// happened to batch.
    ready: VecDeque<SseEvent>,
    delivered: u64,
    last: Option<EventId>,
}

impl fmt::Debug for HttpStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpStream")
            .field("delivered", &self.delivered)
            .field("last", &self.last)
            .field("buffered", &self.reader.buffered())
            .finish_non_exhaustive()
    }
}

impl HttpStream {
    const fn new(response: reqwest::Response) -> Self {
        Self {
            response,
            reader: FrameReader::new(),
            ready: VecDeque::new(),
            delivered: 0,
            last: None,
        }
    }

    /// The next complete frame, or `None` once the peer has finished.
    ///
    /// # Errors
    ///
    /// [`HttpClientError::Frame`] for a terminated block that does not parse,
    /// and [`HttpClientError::Io`] for a transport failure mid-body.
    ///
    /// `None` is not the same as a failure. A body that ended **on a block
    /// boundary** is the peer saying there was nothing more; a body that ended
    /// with bytes still buffered is a connection that dropped mid-event. The
    /// first ends the stream, the second is a resume — which is why
    /// [`HttpStream::has_partial`] exists and why this returns `Option` rather
    /// than erroring on the end.
    pub async fn next_event(&mut self) -> Result<Option<SseEvent>, HttpClientError> {
        loop {
            // Whatever a previous read already assembled is handed over before
            // the socket is touched again. Reading first would reorder the
            // stream past its own buffer, which is a bug that only shows up
            // when a peer is fast.
            if let Some(event) = self.ready.pop_front() {
                return Ok(Some(self.record(event)));
            }
            let chunk = self
                .response
                .chunk()
                .await
                .map_err(|error| HttpClientError::Io { method: GET, error })?;
            let Some(chunk) = chunk else {
                return Ok(None);
            };
            self.ready.extend(self.reader.push(&chunk)?);
        }
    }

    /// Notes a delivered event and its id, so the resume knows where to start.
    const fn record(&mut self, event: SseEvent) -> SseEvent {
        self.delivered = self.delivered.saturating_add(1);
        if let Some(id) = event.id() {
            self.last = Some(id);
        }
        event
    }

    /// How many events this stream has delivered.
    #[must_use]
    pub const fn delivered(&self) -> u64 {
        self.delivered
    }

    /// The id of the last event delivered, which is what a resume asks for.
    #[must_use]
    pub const fn last_event_id(&self) -> Option<EventId> {
        self.last
    }

    /// The id of the last event handed to the caller, or `None` if none has
    /// carried an id.
    ///
    /// A resume starts here, so a caller driving this stream itself — rather
    /// than through [`ResumableStream`], which tracks it privately — has no
    /// other way to learn it. Without this the only way to resume is to give
    /// up the cursor the client already knows and guess.
    #[must_use]
    pub const fn last_delivered(&self) -> Option<EventId> {
        self.last
    }

    /// Whether bytes of an unfinished event are still held.
    #[must_use]
    pub const fn has_partial(&self) -> bool {
        self.reader.buffered() > 0
    }
}

/// A read that reconnects itself, so a dropped stream costs a round trip rather
/// than the session.
#[derive(Debug)]
pub struct ResumableStream<'client> {
    client: &'client HttpClient,
    current: Option<HttpStream>,
    cursor: Option<EventId>,
    delivered: u64,
    attempts: u32,
    require_session: bool,
    policy: Resumable,
    finished: bool,
}

impl ResumableStream<'_> {
    /// The next event, across as many reconnects as the budget allows.
    ///
    /// # Errors
    ///
    /// [`HttpClientError::ResumeExhausted`] once the reconnect budget is spent,
    /// carrying how far the read got and why the last attempt ended. A
    /// non-transient failure — a malformed frame, a media type this client
    /// cannot read, an unparseable session id — is returned as itself, because
    /// reconnecting would produce the same one.
    pub async fn next_event(&mut self) -> Result<Option<SseEvent>, HttpClientError> {
        loop {
            if self.finished {
                return Ok(None);
            }
            // Taken out rather than borrowed in place: the loop body reassigns
            // `current` on a reconnect, and holding a borrow across that is the
            // borrow checker telling the truth about the state machine.
            let mut stream = loop {
                if let Some(stream) = self.current.take() {
                    break stream;
                }
                // A connection that cannot be *established* is the same event
                // as a stream that breaks — a peer that is not answering — and
                // has to be spent against the same budget. Treating only the
                // second as a reconnect means a peer that refuses the first
                // request outright ends the read with a bare `Io`, and the
                // caller cannot tell it apart from a peer that answered and
                // then vanished.
                match self
                    .client
                    .open_stream(self.cursor, self.require_session)
                    .await
                {
                    Ok(opened) => self.current = Some(opened),
                    Err(error) if error.is_transient() => {
                        self.attempts = self.attempts.saturating_add(1);
                        if self.attempts > self.policy.budget.attempts() {
                            return Err(self.exhausted(error));
                        }
                    }
                    Err(error) => return Err(self.widen(error)),
                }
            };

            match stream.next_event().await {
                Ok(Some(event)) => {
                    self.delivered = self.delivered.saturating_add(1);
                    // Read once, into a name. The id decides both where a
                    // resume would start and whether one is possible at all,
                    // and asking twice invites the two answers to disagree.
                    if let Some(id) = event.id() {
                        self.cursor = Some(id);
                    } else {
                        // No id means no cursor, and a peer asked to resume from
                        // the previous id would repeat this event. The read is
                        // therefore finished here, and saying so is the only
                        // honest answer: a resume that guesses is a duplicate.
                        self.finished = true;
                        self.current = None;
                        return Ok(Some(event));
                    }
                    self.current = Some(stream);
                    return Ok(Some(event));
                }
                Ok(None) if stream.has_partial() => {
                    // Ended mid-event. Reconnect from the last delivered id.
                    self.attempts = self.attempts.saturating_add(1);
                    if self.attempts > self.policy.budget.attempts() {
                        return Err(self.exhausted(HttpClientError::StreamEnded {
                            delivered: self.delivered,
                        }));
                    }
                }
                Ok(None) => {
                    // Ended on a boundary: the peer finished. Reconnecting
                    // would ask a peer that has said it is done to say it again.
                    self.finished = true;
                    self.current = None;
                    return Ok(None);
                }
                Err(error) if error.is_transient() => {
                    self.attempts = self.attempts.saturating_add(1);
                    if self.attempts > self.policy.budget.attempts() {
                        return Err(self.exhausted(error));
                    }
                }
                Err(error) => return Err(self.widen(error)),
            }
        }
    }

    /// How many events this read has delivered, across every connection.
    #[must_use]
    pub const fn delivered(&self) -> u64 {
        self.delivered
    }

    /// The id a reconnect would ask for.
    #[must_use]
    pub const fn cursor(&self) -> Option<EventId> {
        self.cursor
    }

    /// How many reconnects have been used.
    #[must_use]
    pub const fn reconnects(&self) -> u32 {
        self.attempts
    }

    /// Wraps a failure that happened after a reconnect, so the caller can see
    /// it had already lost the stream once.
    fn widen(&self, error: HttpClientError) -> HttpClientError {
        if self.attempts == 0 {
            error
        } else {
            HttpClientError::ResumeExhausted {
                attempts: self.attempts,
                delivered: self.delivered,
                source: Box::new(error),
            }
        }
    }

    fn exhausted(&self, source: HttpClientError) -> HttpClientError {
        HttpClientError::ResumeExhausted {
            attempts: self.attempts.saturating_sub(1),
            delivered: self.delivered,
            source: Box::new(source),
        }
    }
}

/// A response that is either one document or an open stream.
#[derive(Debug)]
pub enum HttpReply {
    /// The peer answered with a single JSON document.
    Document(HttpResponse),
    /// The peer answered with an open event stream.
    Stream(HttpStream),
}

/// A fully-read response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    status: reqwest::StatusCode,
    content_type: Option<String>,
    body: Vec<u8>,
    session: Option<McpSessionId>,
}

impl HttpResponse {
    /// The status the peer answered with, always `2xx` — a refusal is an error.
    #[must_use]
    pub const fn status(&self) -> reqwest::StatusCode {
        self.status
    }

    /// The `Content-Type`, with its parameters dropped, or `None` if the peer
    /// sent none.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// The body, read in full.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The session in force when this response arrived.
    #[must_use]
    pub const fn session_id(&self) -> Option<McpSessionId> {
        self.session
    }
}

/// The `Content-Type` with its parameters dropped and its case lowered.
///
/// `text/event-stream; charset=utf-8` is still `text/event-stream`, and a
/// comparison against the whole header value fails the first time a peer adds a
/// parameter. A parameter is ordinary, not hostile.
fn media_type(response: &reqwest::Response) -> Option<String> {
    let raw = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)?
        .to_str()
        .ok()?;
    Some(
        raw.split(';')
            .next()
            .unwrap_or(raw)
            .trim()
            .to_ascii_lowercase(),
    )
}

/// Whether a media type is JSON, accepting the `+json` structured suffix.
fn is_json(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|value| value == JSON || value.ends_with("+json"))
}

/// Whether a media type is an event stream.
fn is_event_stream(content_type: Option<&str>) -> bool {
    content_type == Some(EVENT_STREAM)
}

/// The error for a `2xx` whose body this call cannot read.
///
/// Reads the media type once more rather than taking it, so the call sites that
/// already have it do not have to hand it over and this cannot disagree with
/// them about what the peer sent.
fn media_error(response: &reqwest::Response) -> HttpClientError {
    media_type(response).map_or(HttpClientError::MissingMediaType, |got| {
        HttpClientError::MediaType { got }
    })
}

/// Builds the error for a non-2xx, reading a bounded preview of the body.
async fn refusal(method: &'static str, response: reqwest::Response) -> HttpClientError {
    let status = response.status();
    let content_type = media_type(&response);
    let preview = response
        .bytes()
        .await
        .map_or_else(|_| preview(&[]), |body| preview(&body));
    HttpClientError::Refused {
        method,
        status,
        content_type,
        preview,
    }
}

/// Renders a cursor as a header value, or explains why it cannot be one.
///
/// # Errors
///
/// [`SseError::MalformedId`] if the rendered id is not something a header can
/// carry. Reported as the codec's own error because a cursor that will not fit
/// in a header is a cursor the peer could not have issued either.
fn cursor_value(id: EventId) -> Result<reqwest::header::HeaderValue, HttpClientError> {
    let text = id.to_string();
    if text.len() > MAX_CURSOR {
        return Err(HttpClientError::Frame {
            source: SseError::MalformedId,
        });
    }
    reqwest::header::HeaderValue::from_str(&text).map_err(|_| HttpClientError::Frame {
        source: SseError::MalformedId,
    })
}

/// The `Method` a verb names.
///
/// A `&'static str` is carried through this module so an error can report the
/// verb it was working on, and `Method` is not constructible from one in a
/// `const` context.
///
/// The fallthrough is `GET` because `GET` is the only arm here that is also the
/// zero value a caller can reach without asking: a verb this module did not
/// write cannot produce a request, because the three call sites that pass one
/// pass a constant from this same file.
const fn method_of(verb: &str) -> reqwest::Method {
    match verb.as_bytes() {
        b"POST" => reqwest::Method::POST,
        b"DELETE" => reqwest::Method::DELETE,
        _ => reqwest::Method::GET,
    }
}

impl ClientTransport for HttpClient {
    /// POSTs a request frame and returns the `result` it carries.
    ///
    /// The id matters here for the same reason it matters on stdio: a reply
    /// that arrives for a different request must be recognisable as one, and
    /// this transport carries a session's worth of traffic over one connection.
    ///
    /// # Errors
    ///
    /// [`TransportError::Unavailable`] naming the refusal — a frame this client
    /// could not assemble, a status the transport rejects, or a reply whose
    /// `error` member names a failure.
    fn send(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>> {
        // Assembled before the future is built, because the port elides the
        // return lifetime to `&self` and a boxed future cannot borrow the
        // caller's `method` or `params`.
        let prepared = (|| {
            let id = self.next_request_id();
            let name = MethodName::new(method).map_err(unavailable)?;
            let document = request_document(&id, &name, params).map_err(unavailable)?;
            let envelope = document.as_object().cloned().ok_or_else(|| {
                unavailable(HttpClientError::NotAMessage {
                    got: document.to_string(),
                })
            })?;
            Ok::<_, TransportError>((id, envelope))
        })();

        Box::pin(async move {
            let (id, envelope) = prepared?;
            // `post_envelope` rather than `post`, and that choice is the whole
            // of this method. `post` insists on a whole JSON document, so a
            // server that answers with an event stream — the shape the
            // specification prefers, and the one this crate's own server uses —
            // was rejected outright, and the handshake failed before it began.
            // Going through the envelope reader also parks anything the server
            // volunteered on the way, which a client that only read its own
            // answer had nowhere to put.
            let reply = self
                .post_envelope(&envelope)
                .await
                .map_err(unavailable)?
                .ok_or_else(|| unavailable("a request was answered with no body at all"))?;
            // One re-serialisation to reach the shared `reply_result`, which
            // speaks strings. Taken deliberately over hand-matching the id and
            // lifting `result` here: a second implementation of that rule is a
            // second answer to when a reply counts as this request's.
            let text = serde_json::to_string(&Value::Object(reply)).map_err(unavailable)?;
            reply_result(&text, &id).map_err(unavailable)
        })
    }

    /// POSTs a notification frame and returns without reading a body.
    ///
    /// A notification is answered with a `202` and no content, so there is no
    /// `result` to read and no id to match — asking for one would turn every
    /// correctly-answered notification into a malformed-frame error.
    ///
    /// # Errors
    ///
    /// [`TransportError::Unavailable`] if the frame cannot be assembled or the
    /// `POST` does not succeed.
    fn notify(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let prepared = (|| {
            let name = MethodName::new(method).map_err(unavailable)?;
            let document = notification_document(&name, params).map_err(unavailable)?;
            let Some(envelope) = document.as_object().cloned() else {
                return Err(unavailable(HttpClientError::NotAMessage {
                    got: document.to_string(),
                }));
            };
            Ok::<_, TransportError>(envelope)
        })();

        Box::pin(async move {
            let document = prepared?;
            // `post_envelope` rather than `post`, and this is the notification
            // half of the same fix `send` needs. `post` reads through
            // `read_document`, which refuses an event stream — so a server
            // that answers the closing `notifications/initialized` with one,
            // which the specification allows it to do, failed the handshake on
            // its last step.
            //
            // The envelope's answer is discarded rather than matched: a
            // notification has no id, and the specification answers one with
            // `202` and nothing worth reading. What matters is that the peer
            // accepted it. A frame the server volunteered on the way back is
            // not discarded — `post_envelope` holds it, which is the only
            // reason a client can learn of a push at all.
            self.post_envelope(&document).await.map_err(unavailable)?;
            Ok(())
        })
    }
}

/// This transport's failures, as the port's error.
fn unavailable(error: impl core::fmt::Display) -> TransportError {
    TransportError::Unavailable(error.to_string())
}
