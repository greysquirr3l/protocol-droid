//! Two checks on what the peer sent.
//!
//! [`check_header`] answers whether a request's headers say the peer
//! understands the protocol this build speaks, and
//! [`declares_own_version`] answers whether a body names a revision the
//! server should accept. Neither is a routing decision, and both are small
//! enough to read in one sitting — which is exactly why they are here rather
//! than between the handler's methods.

use crate::adapters::primary::transport::http::refusal::Refusal;
use crate::application::mcp::request_meta::declares_version;
use crate::domain::ProtocolVersion;
use crate::domain::jsonrpc::{Frame, decode};

/// Checks the `MCP-Protocol-Version` header against what the session agreed.
///
/// Only after a handshake, because before one the session has agreed to nothing
/// and there is no answer to compare against.
pub(super) fn check_header(
    header: Option<ProtocolVersion>,
    negotiated: ProtocolVersion,
) -> Result<(), Refusal> {
    let sent = header.map(ProtocolVersion::as_wire);
    crate::domain::check_protocol_header(negotiated, sent.as_deref()).map_err(|error| {
        Refusal::ProtocolHeader {
            reason: error.to_string(),
        }
    })
}


/// Whether a body is a SEP-2575 stateless request.
///
/// The test is the presence of the version key in `params._meta`, which is
/// what makes a request carry its own revision rather than borrow a session's.
/// A batch, a reply, or a body that will not decode has no params to look in
/// and is not a stateless request; the caller's own decode will say why.
pub(super) fn declares_own_version(body: &[u8]) -> bool {
    let Ok(decoded) = decode(body) else {
        return false;
    };
    let Frame::Single(envelope) = decoded else {
        return false;
    };
    let Some(params) = envelope.params() else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(params.get()) else {
        return false;
    };
    declares_version(&parsed)
}

