//! Connecting to a server that has not been told which transport it speaks.
//!
//! [`detect`] answers "which transport is this"; this answers "give me a client
//! to talk to it with", which is the question a caller actually has. It is the
//! two joined, plus the handshake that turns a transport into a session.
//!
//! ## The probe is the handshake, on one path and not the other
//!
//! `detect` needs a request to find out, and the request it sends is an
//! `initialize`. If the peer answers, that request *was* the handshake: its
//! result carries the negotiated version and the server's capabilities, and
//! sending `initialize` a second time would open a second session on a server
//! that is counting them.
//!
//! If the peer answers `4xx`, the probe was never processed — it was a request
//! to a path that does not take requests — and the legacy client that comes
//! back has not been sent anything at all. So that path runs the full
//! [`handshake`] over the connected stream.
//!
//! Both paths end in the same place. What differs between them is not the
//! negotiation but whether the request already happened.

use std::sync::Arc;

use crate::adapters::secondary::transport::detect::{DetectConfig, DetectError, Detected, detect};
use crate::adapters::secondary::transport::http::HttpClientError;
use crate::application::client::{
    Client, ClientConfig, ClientTransport, HandshakeError, INITIALIZED, initialize_params,
    initialize_session, offer,
};
use crate::domain::NegotiatedSession;
use crate::domain::ir::JsonObject;

/// Why a connection could not be established.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The client was built offering no revision, so there was nothing to
    /// negotiate.
    #[error("this client was offered no revision, so there is nothing to negotiate")]
    NothingOffered,

    /// The probe could not be sent, or neither transport answered.
    #[error(transparent)]
    Detect(#[from] DetectError),

    /// The handshake failed.
    ///
    /// Distinct from [`Detect`](Self::Detect) because the peer *did* answer: a
    /// caller who collapsed the two would be sent to check their URL when the
    /// real problem is a revision the two sides do not share.
    #[error(transparent)]
    Handshake(#[from] HandshakeError),

    /// The probe's answer was not an object, so it could not be read as a
    /// handshake result.
    #[error("the server answered the probe with something that is not an object")]
    UnreadableProbe,

    /// The URL could not be turned into a client.
    #[error(transparent)]
    Http(#[from] HttpClientError),

    /// The closing notification could not be delivered.
    ///
    /// Its own variant rather than folded into [`Handshake`](Self::Handshake)
    /// because it happens *after* a session exists. A caller told the handshake
    /// failed would go looking for a version mismatch, and the version
    /// matched fine — the notification is what did not get through.
    #[error("the session was negotiated but the client could not announce it: {0}")]
    NotAnnounced(#[from] crate::application::client::TransportError),
}

/// What a connection ended up speaking, once the transport is known.
///
/// Separate from [`Client`] because the transport is the part a caller cannot
/// get back out of one, and it is the part worth logging: "connected" does not
/// say whether the peer is running the revision this build prefers or the one
/// it fell back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spoke {
    /// The peer speaks Streamable HTTP.
    StreamableHttp,
    /// The peer speaks the `2024-11-05` transport.
    LegacyHttpSse,
}

impl Spoke {
    /// The name a log line or a JSON report wants.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StreamableHttp => "streamable-http",
            Self::LegacyHttpSse => "legacy-http-sse",
        }
    }
}

/// Connects to `url` and negotiates a session, whichever transport answers.
///
/// # Errors
///
/// [`ConnectError::Detect`] if neither transport answered,
/// [`ConnectError::Handshake`] if the peer's answer could not be negotiated into
/// a session, and [`ConnectError::NothingOffered`] for a client configured with
/// no revision.
///
/// # Examples
///
/// ```no_run
/// use protocol_droid::adapters::secondary::transport::connect::{Spoke, connect};
/// use protocol_droid::application::client::ClientConfig;
/// use protocol_droid::domain::{ProtocolVersion, TransportKind};
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let config = ClientConfig::new(ProtocolVersion::V2025_06_18, TransportKind::StreamableHttp);
/// let (client, spoke) = connect("https://example.test/mcp", &config).await?;
/// let tools = client.list_tools().await?;
/// assert!(matches!(spoke, Spoke::StreamableHttp | Spoke::LegacyHttpSse));
/// # Ok(())
/// # }
/// ```
pub async fn connect(url: &str, config: &ClientConfig) -> Result<(Client, Spoke), ConnectError> {
    let (preferred, supported) = offer(config).ok_or(ConnectError::NothingOffered)?;

    // The probe *is* the `initialize` request, so it is built by the same code
    // a handshake would use. A probe hand-built here would be a second
    // definition of what a client's offer looks like.
    let probe = initialize_params(preferred, config)?;
    let detected = detect(DetectConfig::new(url)?, probe.as_str().as_bytes()).await?;

    match detected {
        Detected::Streamable { client, result } => {
            // The probe already asked, so this reads that answer rather than
            // asking a second time.
            let answer = JsonObject::new(String::from_utf8_lossy(&result).into_owned())
                .map_err(|_| ConnectError::UnreadableProbe)?;
            let session = initialize_session(&answer, preferred, &supported, config)?;
            let client = finish(client, session).await?;
            Ok((client, Spoke::StreamableHttp))
        }
        Detected::Legacy(client) => {
            // The probe was refused, so nothing has been sent over the stream
            // yet and the whole handshake still has to happen.
            let session = crate::application::client::handshake(&client, config).await?;
            let client = finish(client, session).await?;
            Ok((client, Spoke::LegacyHttpSse))
        }
    }
}

/// Sends the closing notification and binds the transport to the session.
///
/// The notification goes out on both paths and is not optional: a server that
/// has not been told the client is ready will not answer anything else, so a
/// client that skipped it would look connected and then hang on its first call.
async fn finish<T>(transport: T, session: NegotiatedSession) -> Result<Client, ConnectError>
where
    T: ClientTransport + 'static,
{
    let params = JsonObject::new("{}").map_err(|error| HandshakeError::Assembling {
        method: INITIALIZED,
        source: error,
    })?;
    transport.notify(INITIALIZED, &params).await?;
    Ok(Client::from_session(&session, Arc::new(transport)))
}

#[cfg(all(test, feature = "http"))]
#[path = "connect/tests.rs"]
mod tests;
