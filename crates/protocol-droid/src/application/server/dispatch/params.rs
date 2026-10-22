//! Reading a request's parameters.
//!
//! Three helpers, all of them turning a [`JsonObject`] the peer sent into
//! something typed enough to act on. They are free functions rather than
//! methods because nothing about them depends on which server is dispatching:
//! a `tools/call` params object is the same shape whoever is serving it.
//!
//! They live apart from the router because a JSON read that is wrong is a
//! different bug from a routing decision that is wrong, and `DispatchError` is
//! the only thing the two have in common.

use crate::application::server::builder::DispatchError;
use crate::domain::ir::JsonObject;

/// Checks that `initialize` params are the shape the handshake has to be.
///
/// Every revision that has a handshake marks `capabilities`, `clientInfo` and
/// `protocolVersion` required on `InitializeRequestParams`, and `Implementation`
/// marks `name` and `version` required inside `clientInfo`. Each is read and
/// type-checked here, which is what "not a silently-ignored field" means: a
/// missing or misshapen required field is a refusal, not a default.
///
/// # Why this returns nothing
///
/// A check, not a parse, and the distinction is worth stating because the
/// crate's rule is to parse rather than validate. It returns nothing because
/// nothing in the answer comes from the request: `protocolVersion` is the
/// session's revision, `capabilities` is the session's profile, and
/// `serverInfo` is this server. The offer is a statement about the *client*,
/// and the layer that owns that is the one that negotiated the session — it
/// already holds the client's capability set and, on a `NegotiatedSession`,
/// the client's [`PeerInfo`]. Reading it again here to build a value nobody
/// consumes would be the second source of truth, not the first.
///
/// The one field whose parse *is* a decision: `protocolVersion` is run through
/// [`ProtocolVersion`]'s `FromStr`, so a client offering a revision this build
/// has never heard of — which the specification explicitly allows, and which
/// [`negotiate_server`] answers with the server's own version rather than an
/// error — still gets a session, while a string that is not a `YYYY-MM-DD`
/// date is a typo and is refused.
///
/// # Errors
///
/// Reads `name` and `arguments` out of a `tools/call` params object.
///
/// A missing `name` is a malformed request rather than an unknown tool: the
/// call named nothing at all, and reporting `UnknownTool` for it would send a
/// caller hunting for a tool that exists and works.
///
/// # Errors
///
/// [`DispatchError::MalformedParams`] when the body is not an object, when
/// `name` is absent or is not a string, and when `arguments` is present but is
/// not an object. `arguments` is optional — a tool taking no parameters is
/// called with none.
pub(super) fn split_call_params(
    params: &JsonObject,
) -> Result<(String, JsonObject), DispatchError> {
    let parsed: serde_json::Value =
        serde_json::from_str(params.as_str()).map_err(|_| DispatchError::MalformedParams)?;
    let object = parsed.as_object().ok_or(DispatchError::MalformedParams)?;

    let name = object
        .get("name")
        .and_then(serde_json::Value::as_str)
        .ok_or(DispatchError::MalformedParams)?
        .to_string();

    let arguments = match object.get("arguments") {
        None | Some(serde_json::Value::Null) => JsonObject::default(),
        Some(arguments) => {
            JsonObject::from_value(arguments.clone()).map_err(|_| DispatchError::MalformedParams)?
        }
    };

    Ok((name, arguments))
}

/// The string member `key` of a params object.
pub(super) fn string_param(params: &JsonObject, key: &str) -> Result<String, DispatchError> {
    parsed_param(params, key)?
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or(DispatchError::MalformedParams)
}

/// One member of a params object, already parsed.
///
/// `None` for an absent member as well as `null`, because both mean the same
/// thing to every caller here and the specification's params are optional
/// throughout. A non-object params document is malformed either way.
pub(super) fn parsed_param(
    params: &JsonObject,
    key: &str,
) -> Result<Option<serde_json::Value>, DispatchError> {
    let parsed: serde_json::Value =
        serde_json::from_str(params.as_str()).map_err(|_| DispatchError::MalformedParams)?;
    let object = parsed.as_object().ok_or(DispatchError::MalformedParams)?;
    Ok(object.get(key).cloned())
}
