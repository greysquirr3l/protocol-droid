//! The `2024-11-05` HTTP+SSE transport, which the specification replaced in
//! `2025-03-26` and which is still the only transport that version speaks.
//!
//! # The wire shape, from the specification
//!
//! The server **MUST** provide two endpoints:
//!
//! 1. An SSE endpoint, for clients to establish a connection and receive
//!    messages from the server.
//! 2. A regular HTTP POST endpoint for clients to send messages to the server.
//!
//! > When a client connects, the server MUST send an `endpoint` event
//! > containing a URI for the client to use for sending messages. All
//! > subsequent client messages MUST be sent as HTTP POST requests to this
//! > endpoint.
//!
//! > Server messages are sent as SSE `message` events, with the message content
//! > encoded as JSON in the event data.
//!
//! Cited: <https://modelcontextprotocol.io/specification/2024-11-05/basic/transports>
//!
//! # What the specification does not say, and what this crate does anyway
//!
//! Two things every implementation does and the text does not mention:
//!
//! - **A session id in the endpoint URI.** The server chooses the URI, and
//!   this crate puts a `sessionId` query parameter on it because the POST
//!   endpoint has to be able to tell which open stream a message belongs to.
//!   That is a design choice with a spec-compatible spec, not a spec
//!   requirement — see [`MessageEndpoint`], which exists partly to make the
//!   difference visible.
//! - **`202 Accepted` on the POST.** A POST's answer arrives on the SSE
//!   stream, not in the response, so the response has nothing to say and says
//!   so. Also a convention. The specification is silent, and a client that
//!   depends on the status would be depending on something the text does not
//!   promise.
//!
//! # The client's half, which the specification does describe
//!
//! From `2025-03-26`, the revision that replaced this transport, the
//! Backwards Compatibility section:
//!
//! > 1. Issue an HTTP POST to the MCP server endpoint. If it succeeds, proceed
//! >    as normal with Streamable HTTP.
//! > 2. If it fails with an HTTP 4xx status code (e.g., 405 Method Not Allowed
//! >    or 404 Not Found): Issue a GET request to the server URL, expecting
//! >    that this will open an SSE stream and return an `endpoint` event as the
//! >    first event. When the `endpoint` event arrives, the client can assume
//! >    this is a server running the old HTTP+SSE transport, and should use
//! >    that transport for all subsequent communication.
//!
//! Cited: <https://modelcontextprotocol.io/specification/2025-03-26/basic/transports>
//!
//! The 4xx, not any failure, is the trigger. A 5xx is a server having a bad
//! day and should be retried the same way next time; only a 4xx says the
//! server is not speaking Streamable HTTP at all.
//!
//! # Why this is behind a feature gate
//!
//! Because it is deprecated, and `default-features = false` is how a consumer
//! says what they actually link. A server that will only ever speak a current
//! revision has no use for an `endpoint` event, a second endpoint, or the
//! `Origin` allowlist — and `TransportKind::LegacySse` already exists in the
//! domain, ungated, because *describing* the transport costs nothing and
//! only *speaking* it does.

mod frame;
mod router;
mod serve;

pub use crate::adapters::primary::transport::origin::{Origin, OriginError, OriginPolicy};
pub use frame::{LegacyDispatcher, LegacyFrame, LegacySession, LegacyTail};
pub use serve::{ServerDispatcher, ServerSession};
pub use router::{
    DEFAULT_MESSAGE_PATH, DEFAULT_STREAM_PATH, LegacyRouter, RouteError, SESSION_PARAMETER,
};

pub use crate::domain::{MessageEndpoint, MessageEndpointError};
