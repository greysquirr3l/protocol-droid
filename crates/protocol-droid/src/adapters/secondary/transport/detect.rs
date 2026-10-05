//! Which HTTP transport is this server speaking?
//!
//! The `2025-03-26` specification's backwards-compatibility section says:
//!
//! > **Clients** wanting to support older servers should:
//! >
//! > 1. Accept an MCP server URL from the user, which may point to either a
//! >    server using the old transport or the new transport.
//! > 2. Attempt to POST an `InitializeRequest` to the server URL, with an
//! >    `Accept` header as defined above:
//! >    * If it succeeds, the client can assume this is a server supporting the
//! >      new Streamable HTTP transport.
//! >    * If it fails with an HTTP 4xx status code (e.g., 405 Method Not Allowed
//! >      or 404 Not Found):
//! >      * Issue a GET request to the server URL, expecting that this will open
//! >        an SSE stream and return an `endpoint` event as the first event.
//! >      * When the `endpoint` event arrives, the client can assume this is a
//! >        server running the old HTTP+SSE transport, and should use that
//! >        transport for all subsequent communication.
//!
//! Four things in that are load-bearing and each is a place an implementation
//! goes wrong in a way no test against a real server will catch:
//!
//! - **4xx, not "not 2xx".** A `5xx` is a server that answered and failed. It
//!   speaks the new transport and something is broken; falling back would open a
//!   second transport on top of a real error and report the wrong cause.
//! - **`GET` to the *same* URL.** The client does not know where the SSE
//!   endpoint is. The server names it in the `endpoint` event, and a client that
//!   guesses `/sse` is relying on a convention the specification never states.
//! - **`endpoint` must be *first*.** A server that opens with a `message` frame
//!   is one that skipped the handshake; there is nowhere to post, so the client
//!   has to stop rather than guess.
//! - **The decision is made once.** "for all subsequent communication" is a
//!   direction to a client, not a suggestion to re-probe per request.

use core::time::Duration;

use thiserror::Error;

use crate::adapters::secondary::transport::http::{HttpClient, HttpClientConfig, HttpClientError};
use crate::adapters::secondary::transport::legacy_sse::{
    LegacySseClient, LegacySseConfig, LegacySseError,
};
use crate::domain::TransportKind;

/// The `Accept` header the specification names, verbatim.
const ACCEPT: &str = "application/json, text/event-stream";
/// The media type the probe is sent as.
const JSON: &str = "application/json";

/// Why a server could not be classified.
#[derive(Debug, Error)]
pub enum DetectError {
    /// The probe request never reached the peer.
    #[error("the probe could not be sent: {error}")]
    Io {
        /// The transport's own account of the failure.
        #[source]
        error: reqwest::Error,
    },

    /// The probe was answered with a status the rule does not cover.
    ///
    /// Its own variant rather than a bare `HttpClientError`, because *which*
    /// status it was is the entire decision and a caller that gets it as a
    /// string has to re-derive the branch the rule already made. A `5xx` is the
    /// case that matters: the peer answered, it speaks the new transport, and
    /// something on it is broken.
    #[error(
        "the server answered the probe with {status}, which is not a version signal and not a success"
    )]
    Unusable {
        /// The status the peer answered with.
        status: reqwest::StatusCode,
        /// The first 200 characters of the body, decoded lossily.
        preview: String,
    },

    /// The peer answered 4xx and then would not open a stream either.
    ///
    /// The two halves of the rule both ran and neither produced a transport,
    /// which means the URL is not an MCP server at all. Reporting "no
    /// transport" without saying which half failed would send a reader looking
    /// at the wrong transport.
    #[error("the server refused the probe with {status} and then would not open an event stream")]
    Neither {
        /// The status that triggered the fallback.
        status: reqwest::StatusCode,
        /// What the `GET` half said.
        #[source]
        source: LegacySseError,
    },
}

/// A server that answered, and the client that talks to it.
#[derive(Debug)]
pub enum Detected {
    /// The probe succeeded: the server speaks Streamable HTTP.
    Streamable {
        /// A client carrying the session the probe's response issued.
        client: HttpClient,
        /// The probe's response body — the `initialize` result, which the caller
        /// has to read and this client has already consumed the headers of.
        result: Vec<u8>,
    },
    /// The probe was refused and a stream opened: the server speaks `2024-11-05`.
    ///
    /// A connected client rather than a verdict, because the `endpoint` event has
    /// already arrived and been checked by the time this is constructed. A
    /// caller reaching this arm has a working transport, not a finding to act on.
    Legacy(LegacySseClient),
}

impl Detected {
    /// Which transport the server speaks.
    ///
    /// Decided once and recorded, so a caller that wants to log it does not
    /// re-run the branch — and so the two arms cannot disagree.
    #[must_use]
    pub const fn transport(&self) -> TransportKind {
        match self {
            Self::Streamable { .. } => TransportKind::StreamableHttp,
            Self::Legacy(_) => TransportKind::LegacySse,
        }
    }

    /// The `2024-11-05` client, if that is what the server speaks.
    ///
    /// # Errors
    ///
    /// [`NotLegacy`] if the server answered the probe, in which case the answer
    /// is [`Detected::Streamable`] and this is a caller that took the wrong
    /// branch for the transport it was handed.
    pub fn into_legacy(self) -> Result<LegacySseClient, NotLegacy> {
        match self {
            Self::Legacy(client) => Ok(client),
            Self::Streamable { .. } => Err(NotLegacy::NotLegacy),
        }
    }
}

/// Why [`Detected::into_legacy`] refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NotLegacy {
    /// The server answered the probe and so speaks the new transport.
    #[error("the server speaks Streamable HTTP, not the 2024-11-05 transport")]
    NotLegacy,
}

/// Where to probe and how patient to be.
#[derive(Debug, Clone)]
pub struct DetectConfig {
    /// Validated once, here, and never re-derived.
    ///
    /// Held as the typed config rather than as a `String` so that handing it to
    /// a client at the end is a move rather than a re-parse that could fail —
    /// and fail only on a value this function did not produce.
    config: HttpClientConfig,
    timeout: Option<Duration>,
}

impl DetectConfig {
    /// A configuration pointed at `url`.
    ///
    /// # Errors
    ///
    /// [`HttpClientError::Url`] if `url` is not a URL a request can be built
    /// against.
    pub fn new(url: &str) -> Result<Self, HttpClientError> {
        Ok(Self {
            config: HttpClientConfig::new(url)?,
            timeout: None,
        })
    }

    /// Bounds how long the probe may take.
    ///
    /// Applies to the probe **only**. The `GET` half opens a stream that is
    /// supposed to stay open, so a request timeout handed to it would break
    /// every connection longer than it — see
    /// [`LegacySseConfig::with_connect_timeout`], which is why this config has
    /// a separate one.
    #[must_use]
    pub const fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// Asks a server which transport it speaks, and returns a client that can.
///
/// `probe` is the serialised `InitializeRequest`. It is a parameter rather than
/// something this module builds because the protocol layer owns what an
/// initialize request is and which version it asks for; a transport that
/// hard-coded one would be encoding a version-negotiation decision inside a
/// module whose job is moving bytes.
///
/// # Errors
///
/// [`DetectError::Io`] if the probe never arrived,
/// [`DetectError::Unusable`] if the peer answered with a status that is neither
/// a success nor a 4xx — a `5xx` included, deliberately — and
/// [`DetectError::Neither`] if the peer answered 4xx and then would not open a
/// stream.
pub async fn detect(config: DetectConfig, probe: &[u8]) -> Result<Detected, DetectError> {
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("protocol-droid/", env!("CARGO_PKG_VERSION")));
    if let Some(limit) = config.timeout {
        builder = builder.timeout(limit);
    }
    let inner = builder.build().map_err(|error| DetectError::Io { error })?;

    let response = inner
        .post(config.config.endpoint())
        .header(reqwest::header::ACCEPT, ACCEPT)
        .header(reqwest::header::CONTENT_TYPE, JSON)
        .body(probe.to_vec())
        .send()
        .await
        .map_err(|error| DetectError::Io { error })?;

    let status = response.status();
    if status.is_client_error() {
        drop(response);
        // The rule's second half, and only for a 4xx. The `GET` goes to the
        // same URL — this module does not know, and is not entitled to guess,
        // where the SSE endpoint is.
        return fall_back(inner, config, status).await;
    }
    if !status.is_success() {
        return Err(DetectError::Unusable {
            status,
            preview: preview(response).await,
        });
    }

    // The probe's own response is the `initialize` result, and its headers carry
    // the session every later request needs. Both are read here because the
    // response is consumed below and a client built without the session would
    // re-run the handshake to get it.
    let session = response
        .headers()
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let result = response
        .bytes()
        .await
        .map_err(|error| DetectError::Io { error })?
        .to_vec();

    // The URL was checked by `DetectConfig::new` and has not changed since, so
    // it cannot fail here. Re-deriving it through `expect`-shaped code would add
    // an arm that is unreachable rather than remove one.
    let mut client = HttpClient::from_client(inner, config.config.clone());
    if let Some(session) = session {
        client = client.resuming(session);
    }
    Ok(Detected::Streamable { client, result })
}

/// The 4xx branch, which is not a second implementation but a call to the one.
async fn fall_back(
    inner: reqwest::Client,
    config: DetectConfig,
    status: reqwest::StatusCode,
) -> Result<Detected, DetectError> {
    let mut legacy =
        LegacySseConfig::new(config.config.endpoint()).map_err(|source| DetectError::Neither {
            status,
            source: unclassifiable(source),
        })?;
    if let Some(limit) = config.timeout {
        legacy = legacy.with_connect_timeout(limit);
    }
    LegacySseClient::connect_with(inner, legacy)
        .await
        .map(Detected::Legacy)
        .map_err(|source| DetectError::Neither { status, source })
}

/// Wraps a configuration failure as something the `Neither` arm can carry.
///
/// The only way to get here is a URL that [`DetectConfig::new`] already
/// accepted and this module then failed to accept a second time, so it does not
/// carry a `Url` variant of its own to keep in step. It is kept rather than
/// unwrapped so the `Neither` arm's type stays about the stream, which is the
/// thing that actually failed.
fn unclassifiable(source: LegacySseError) -> LegacySseError {
    match source {
        LegacySseError::Url { got, reason } => LegacySseError::Unreadable { got, reason },
        other => other,
    }
}

/// The header carrying the session on this transport and every later one.
const SESSION_HEADER: &str = "mcp-session-id";

/// A body, bounded, for an error that has to show what the peer said.
async fn preview(response: reqwest::Response) -> String {
    response
        .bytes()
        .await
        .map(|body| String::from_utf8_lossy(&body).chars().take(200).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
