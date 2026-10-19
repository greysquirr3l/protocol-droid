//! The `2024-11-05` client: a `GET` that opens a stream, and a `POST` to
//! wherever the server says.
//!
//! The shape is the inverse of every other transport here, and the inversion is
//! worth stating once. On Streamable HTTP the client owns the URL and the server
//! answers where it was asked. Here the **server owns the URL**: the client
//! `GET`s something, and the first thing that comes back is the path every later
//! request must go to. A client that trusted that path would be a server
//! choosing where the client authenticates. This one parses it through
//! [`MessageEndpoint`], which refuses a target naming another host, and both
//! that refusal and the missing handshake are tested rather than asserted.

use core::future::Future;
use core::pin::Pin;
use core::str::FromStr;
use core::sync::atomic::{AtomicI64, Ordering};
use core::time::Duration;
use tokio::sync::Mutex;

use serde_json::Value;

use crate::adapters::primary::transport::http::SseEvent;
use crate::adapters::secondary::transport::http::FrameReader;
use crate::application::jsonrpc::document::{
    notification_document, reply_result, request_document,
};
use crate::domain::ir::JsonObject;
use crate::domain::{MessageEndpoint, RequestId, names};

use crate::application::client::{ClientTransport, TransportError};
use crate::application::jsonrpc::document::RequestError;
use crate::domain::ir::MethodName;

use super::error::{self, LegacySseError};

/// The media type that means "an open event stream".
const EVENT_STREAM: &str = "text/event-stream";
/// The media type this transport POSTs.
const JSON: &str = "application/json";

/// The HTTP method the connection is opened with, named in errors.
const GET: &str = "GET";
/// The HTTP method every client write uses.
const POST: &str = "POST";

/// Where a `2024-11-05` client connects and how patient it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySseConfig {
    url: String,
    connect_timeout: Option<Duration>,
}

impl LegacySseConfig {
    /// A configuration pointed at `url`.
    ///
    /// # Errors
    ///
    /// [`LegacySseError::Url`] if `url` is not a URL a request can be built
    /// against. Checked here rather than at connect time, so a typo is a
    /// construction error and not a surprise on the wire.
    pub fn new(url: &str) -> Result<Self, LegacySseError> {
        reqwest::Url::parse(url).map_err(|error| LegacySseError::Url {
            got: url.to_owned(),
            reason: error.to_string(),
        })?;
        Ok(Self {
            url: url.to_owned(),
            connect_timeout: None,
        })
    }

    /// Bounds how long the connection may take to open.
    ///
    /// A **connect** timeout, not a request one, and the distinction is the
    /// point. A request timeout would bound the whole read, and on this
    /// transport the read *is* the conversation: a stream meant to stay open for
    /// an hour is a correct client, not a slow one. Bounding the read would
    /// break every session longer than the timeout and report it as a failure
    /// naming a request that was never slow.
    #[must_use]
    pub const fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// The URL this client was pointed at.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// One frame off the stream, already classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientFrame {
    /// A JSON-RPC frame the server sent.
    Message(Value),
    /// An `endpoint` event.
    ///
    /// Kept in the enum rather than consumed by the handshake, because a client
    /// that reconnects sees the handshake again — and a client with no case for
    /// it drops the one frame that tells it where to post.
    Endpoint(MessageEndpoint),
}

impl ClientFrame {
    /// Classifies a frame off the wire.
    ///
    /// The unnamed case is the load-bearing one. The specification calls these
    /// `message` events, and an SSE event with no `event:` field **is** a
    /// `message` event by the standard's own default — which is what this
    /// crate's server writes. So the question is never "is it named
    /// `message`" but "is it named something *else*". A client filtering on
    /// `is_named(&names::MESSAGE)` and nothing else passes every test written
    /// against a frame explicitly named `message`, and drops every frame a
    /// conforming server actually sends.
    fn classify(event: &SseEvent) -> Result<Self, LegacySseError> {
        let data = event.data().to_owned();
        if event.is_named(&names::ENDPOINT) {
            return MessageEndpoint::from_str(&data)
                .map(Self::Endpoint)
                .map_err(LegacySseError::endpoint);
        }
        // Everything else is a payload — including a frame explicitly named
        // `message`, which some 2024-11-05 SDKs do send, and which is the same
        // frame under a different spelling.
        serde_json::from_str::<Value>(&data)
            .map(Self::Message)
            .map_err(|error| LegacySseError::Unreadable {
                got: truncate(&data),
                reason: error.to_string(),
            })
    }

    /// What this frame was, for a message that has to name it.
    fn describe(&self) -> String {
        match self {
            Self::Endpoint(endpoint) => format!("an `endpoint` event naming {endpoint}"),
            Self::Message(value) => format!("a message frame: {value}"),
        }
    }
}

/// The open stream, and the machinery that turns its bytes into frames.
///
/// Split from [`LegacySseClient`] so the handshake can read the first event
/// *before* the client that will hold it exists. That ordering is what lets the
/// message endpoint be a plain field rather than an `Option` every `send` has to
/// unwrap: there is no state in which a client is built and its post target is
/// still unknown.
#[derive(Debug)]
struct Stream {
    response: reqwest::Response,
    reader: FrameReader,
    ready: std::collections::VecDeque<SseEvent>,
}

impl Stream {
    /// The next complete frame, or `None` once the peer has finished.
    async fn next(&mut self) -> Result<Option<SseEvent>, LegacySseError> {
        loop {
            // Handed over before the socket is touched again. Reading first
            // would reorder the stream past its own buffer, which only shows up
            // when a peer is fast.
            if let Some(event) = self.ready.pop_front() {
                return Ok(Some(event));
            }
            let chunk = self
                .response
                .chunk()
                .await
                .map_err(|error| LegacySseError::Io { method: GET, error })?;
            let Some(chunk) = chunk else {
                return Ok(None);
            };
            self.ready
                .extend(
                    self.reader
                        .push(&chunk)
                        .map_err(|source| LegacySseError::Unreadable {
                            got: String::from("a terminated SSE block"),
                            reason: source.to_string(),
                        })?,
                );
        }
    }
}

/// A connected `2024-11-05` client.
///
/// The stream is open from the moment this exists and stays open for its life:
/// this transport has no session header and no reconnection story, so closing
/// the `GET` is the only way to end the conversation. That is why the send path takes
/// `&self` but [`next`](Self::next) takes `&mut self` — a `POST` is a write on a
/// different request and does not consume the reader.
#[derive(Debug)]
pub struct LegacySseClient {
    inner: reqwest::Client,
    config: LegacySseConfig,
    /// Where every `POST` goes.
    ///
    /// Resolved, not the raw advertised target. The target arrives relative —
    /// that is what [`MessageEndpoint`] insists on, because a relative target
    /// has no host in it to redirect through — and a relative target is not
    /// something a request can be sent to. Resolution is where the origin check
    /// lives, and [`resolve`] is where it is proved.
    post_to: reqwest::Url,
    /// Boxed, and only because of [`Detected`](crate::adapters::secondary::transport::detect::Detected).
    ///
    /// The open response and its buffer are far larger than the other arm of
    /// that enum, so an unboxed stream makes every `Detected` as big as the
    /// legacy one. Nothing else changes: the stream is open for the client's
    /// whole life and is never moved.
    ///
    /// Behind a [`Mutex`] because [`ClientTransport`] sends from `&self`: a
    /// client that could only be read from `&mut self` could not sit behind
    /// `Arc<dyn ClientTransport>`, which is the only shape a negotiated
    /// [`Client`](crate::application::client::Client) can hold.
    ///
    /// `tokio`'s rather than `std`'s, and not for convenience: a frame is read
    /// across an `await`, and `std::sync::MutexGuard` is `!Send`, so the whole
    /// future would stop being `Send` — and the port requires a `Send` future.
    /// Held across the read on purpose: a frame cannot be read from two places
    /// at once, and a lock dropped at the first `await` would let a second call
    /// interleave into the middle of the first one's read.
    ///
    /// [`Mutex`]: tokio::sync::Mutex
    stream: Mutex<Box<Stream>>,

    /// The id to put on the next request, so replies can be matched to the
    /// request that asked for them.
    ///
    /// This transport carries many requests over one stream, so a reply arrives
    /// with no request attached — matching the id is the only thing that says
    /// which of the in-flight asks a given frame answers.
    next_id: AtomicI64,
}

impl LegacySseClient {
    /// Opens the stream and completes the handshake.
    ///
    /// The `GET` goes to the configured URL and the first event is required to
    /// be an `endpoint`. Everything after this is driven by what the server
    /// named, which is both the point of the transport and its only untrusted
    /// input.
    ///
    /// # Errors
    ///
    /// [`LegacySseError::Io`] if the socket failed, [`LegacySseError::Refused`]
    /// if the peer answered outside 2xx, [`LegacySseError::NotAStream`] if a 2xx
    /// was not an event stream, [`LegacySseError::Unreadable`] if a frame could
    /// not be read, [`LegacySseError::FirstEventNotEndpoint`] if the handshake
    /// was missing, and [`LegacySseError::Endpoint`] if the handshake named a
    /// target this client refuses to post to.
    pub async fn connect(config: LegacySseConfig) -> Result<Self, LegacySseError> {
        let inner = build_client(&config)?;
        Self::connect_with(inner, config).await
    }

    /// Opens the stream on a connection pool that already exists.
    ///
    /// For the auto-detect step, which must hold a client before it knows which
    /// transport the peer speaks — the probe request is the thing that tells it.
    /// Handing the pool to whichever client wins means one TLS handshake and one
    /// DNS resolution for the whole connection.
    ///
    /// # Errors
    ///
    /// Exactly as [`LegacySseClient::connect`].
    pub async fn connect_with(
        inner: reqwest::Client,
        config: LegacySseConfig,
    ) -> Result<Self, LegacySseError> {
        let mut stream = open(&inner, &config.url).await?;

        // The handshake, read before the client exists. A failed handshake
        // therefore leaves nothing a caller could send with — there is no
        // half-built client to leak out of this function.
        let first = match stream.next().await? {
            Some(event) => ClientFrame::classify(&event)?,
            None => {
                return Err(LegacySseError::FirstEventNotEndpoint {
                    got: String::from("an empty stream"),
                });
            }
        };
        let ClientFrame::Endpoint(advertised) = first else {
            return Err(LegacySseError::FirstEventNotEndpoint {
                got: first.describe(),
            });
        };
        let post_to = resolve(&config.url, &advertised)?;

        Ok(Self {
            inner,
            config,
            post_to,
            stream: Mutex::new(Box::new(stream)),
            next_id: AtomicI64::new(1),
        })
    }

    /// Where this client posts: the server's target, resolved against the URL
    /// the client dialled.
    #[must_use]
    pub const fn message_endpoint(&self) -> &reqwest::Url {
        &self.post_to
    }

    /// The configuration this client was built with.
    #[must_use]
    pub const fn config(&self) -> &LegacySseConfig {
        &self.config
    }

    /// POSTs one JSON-RPC frame to the endpoint the server named.
    ///
    /// The answer does not come back here. On this transport it arrives as a
    /// `message` event on the stream, which is why a `2024-11-05` server answers
    /// `202` with an empty body — and why a caller treating a successful return
    /// as "the request was handled" is right on every other transport and wrong
    /// on this one.
    ///
    /// # Errors
    ///
    /// [`LegacySseError::Io`] if the socket failed, or
    /// [`LegacySseError::Refused`] if the peer answered outside 2xx.
    pub async fn send(&self, frame: &[u8]) -> Result<(), LegacySseError> {
        let response = self
            .inner
            .post(self.post_to.clone())
            .header(reqwest::header::CONTENT_TYPE, JSON)
            .body(frame.to_vec())
            .send()
            .await
            .map_err(|error| LegacySseError::Io {
                method: POST,
                error,
            })?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(refusal(POST, response).await)
        }
    }

    /// The next frame off the stream, or `None` when the peer closed it.
    ///
    /// # Errors
    ///
    /// [`LegacySseError::Io`] if the socket failed mid-read, or
    /// [`LegacySseError::Unreadable`] if a frame was neither an `endpoint` event
    /// nor a JSON payload.
    pub async fn next(&self) -> Result<Option<ClientFrame>, LegacySseError> {
        let mut stream = self.stream.lock().await;
        stream
            .next()
            .await?
            .map(|event| ClientFrame::classify(&event))
            .transpose()
    }

    /// The id for the next request on this connection.
    pub(crate) fn next_request_id(&self) -> RequestId {
        RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed))
    }
}

/// Resolves an advertised target against the URL the client dialled.
///
/// The advertised target is origin-relative by construction — that is what
/// [`MessageEndpoint`] refuses to be anything else — so the join cannot leave
/// the origin. The check is written anyway, and it earns its place: it is what
/// verifies *this* join rather than what verifies the parser, and it is the thing
/// that fires if someone later replaces the join with string concatenation. A
/// test cannot drive the branch, so the property it stands for is asserted
/// directly in this module's tests instead.
fn resolve(base: &str, advertised: &MessageEndpoint) -> Result<reqwest::Url, LegacySseError> {
    let base = reqwest::Url::parse(base).map_err(|error| LegacySseError::Url {
        got: base.to_owned(),
        reason: error.to_string(),
    })?;
    let resolved = base
        .join(advertised.as_str())
        .map_err(|error| LegacySseError::Url {
            got: advertised.as_str().to_owned(),
            reason: error.to_string(),
        })?;
    if resolved.origin() != base.origin() {
        return Err(LegacySseError::Url {
            got: advertised.as_str().to_owned(),
            // `Origin` is opaque and does not render, so the check compares it
            // and the message names the URLs. Same claim, printable.
            reason: format!(
                "it resolves to {resolved}, which is not on the origin this client was given ({base})"
            ),
        });
    }
    Ok(resolved)
}

/// Opens the stream and refuses anything that is not one.
async fn open(inner: &reqwest::Client, url: &str) -> Result<Stream, LegacySseError> {
    let response = inner
        .get(url)
        .header(reqwest::header::ACCEPT, EVENT_STREAM)
        .send()
        .await
        .map_err(|error| LegacySseError::Io { method: GET, error })?;
    if !response.status().is_success() {
        return Err(refusal(GET, response).await);
    }
    if !is_event_stream(&response) {
        return Err(LegacySseError::NotAStream {
            got: response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Ok(Stream {
        response,
        reader: FrameReader::new(),
        ready: std::collections::VecDeque::new(),
    })
}

/// Whether the peer answered with an open event stream.
fn is_event_stream(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case(EVENT_STREAM))
        })
}

/// A pool configured the way this transport needs.
fn build_client(config: &LegacySseConfig) -> Result<reqwest::Client, LegacySseError> {
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("protocol-droid/", env!("CARGO_PKG_VERSION")));
    if let Some(limit) = config.connect_timeout {
        builder = builder.connect_timeout(limit);
    }
    builder.build().map_err(|error| LegacySseError::Io {
        method: "CLIENT",
        error,
    })
}

/// A peer-supplied status, rendered for an error without keeping the whole body.
async fn refusal(method: &'static str, response: reqwest::Response) -> LegacySseError {
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let preview = response
        .bytes()
        .await
        .map(|body| {
            String::from_utf8_lossy(&body)
                .chars()
                .take(error::PREVIEW)
                .collect()
        })
        .unwrap_or_default();
    LegacySseError::Refused {
        method,
        status,
        content_type,
        preview,
    }
}

/// A peer-supplied string, bounded before it goes in an error.
fn truncate(value: &str) -> String {
    value.chars().take(error::PREVIEW).collect()
}

#[cfg(test)]
mod tests;

impl ClientTransport for LegacySseClient {
    /// POSTs a request frame and waits for the reply that carries its id.
    ///
    /// A `202` on the `POST` is not the answer: on this transport the answer
    /// goes to the stream the `GET` opened, and it arrives there interleaved
    /// with anything else the server chooses to send. So this writes the frame,
    /// then reads until a frame with this request's id turns up.
    ///
    /// # Errors
    ///
    /// [`TransportError::Unavailable`] naming the refusal. The port is
    /// protocol-neutral and must not learn that this is a legacy transport, so
    /// the detail is rendered rather than kept.
    fn send(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>> {
        let prepared = (|| {
            let id = self.next_request_id();
            let name = method_name(method)?;
            let document = request_document(&id, &name, params)
                .map_err(unavailable)?
                .to_string();
            Ok::<_, TransportError>((id, document))
        })();

        Box::pin(async move {
            let (id, document) = prepared?;
            self.send(document.as_bytes()).await.map_err(unavailable)?;
            self.await_reply(&id).await
        })
    }

    /// POSTs a notification frame and returns without reading.
    ///
    /// No read, and that is the whole difference from
    /// [`FrameSink::send`](crate::application::session::frame_sink::FrameSink::send). A notification
    /// has no id, so there is nothing to match a frame against, and reading one
    /// anyway would consume a frame that belongs to whatever asked next — which
    /// on this transport is every reply that arrives in between.
    ///
    /// # Errors
    ///
    /// [`TransportError::Unavailable`] if the frame cannot be built or written.
    fn notify(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let prepared = (|| {
            let name = method_name(method)?;
            let document = notification_document(&name, params)
                .map_err(unavailable)?
                .to_string();
            Ok::<_, TransportError>(document)
        })();

        Box::pin(async move {
            let document = prepared?;
            self.send(document.as_bytes()).await.map_err(unavailable)
        })
    }
}

impl LegacySseClient {
    /// Reads the stream until the frame answering `id` arrives.
    ///
    /// Reads rather than takes the next one. The stream carries the server's
    /// own traffic as well as answers — a `notifications/message` for a tool
    /// call, a progress update for a request that is still running — and a
    /// reader that took the first frame would hand the caller a notification
    /// where it asked for a result.
    async fn await_reply(&self, id: &RequestId) -> Result<JsonObject, TransportError> {
        loop {
            let Some(frame) = self.next().await.map_err(unavailable)? else {
                return Err(TransportError::Unavailable(format!(
                    "the stream closed before answering {:?}",
                    id.redacted()
                )));
            };
            match frame {
                ClientFrame::Message(payload) => {
                    if let Some(answer) = reply_for(&payload, id) {
                        return answer.map_err(unavailable);
                    }
                }
                // An endpoint frame is the server re-advertising its target. It
                // has no id, and there is nothing a client can do with a second
                // one, so it is skipped rather than treated as a failure.
                ClientFrame::Endpoint(_) => {}
            }
        }
    }
}

/// The `result` of `payload` when it answers `id`.
fn reply_for(payload: &Value, id: &RequestId) -> Option<Result<JsonObject, RequestError>> {
    let members = payload.as_object()?;
    // `to_json` is the id as JSON text — `1`, or `"a"` with its quotes — so it
    // is parsed before it is compared. Wrapping the text in a `Value::String`
    // matched only a server that answered a numeric id with a string, which is
    // no server.
    let wanted: Option<Value> = serde_json::from_str(&id.to_json()).ok();
    if wanted.as_ref() != members.get("id") {
        return None;
    }
    // `reply_result` re-parses what was just walked, which is a second pass over
    // a small document and buys the one implementation of id matching and error
    // reporting that the stdio transport already has tested. Matching the id
    // here rather than inside it is what lets a notification — which has no id
    // at all — be skipped instead of being reported as a mismatch.
    Some(reply_result(&payload.to_string(), id))
}

/// A method name, or the port's own error.
fn method_name(method: &str) -> Result<MethodName, TransportError> {
    MethodName::new(method).map_err(unavailable)
}

/// This transport's failures, as the port's error.
fn unavailable(error: impl core::fmt::Display) -> TransportError {
    TransportError::Unavailable(error.to_string())
}
