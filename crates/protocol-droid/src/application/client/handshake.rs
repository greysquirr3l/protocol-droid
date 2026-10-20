//! Establishing a session with a server, over whatever transport.
//!
//! Split from [`Client`](crate::application::client::Client) because a client and a session are not
//! the same thing: the client is what a caller holds, and it cannot be built
//! until this has run. The crate had both halves and nothing joining them, so
//! every caller that wanted a connected client either took a transport on trust
//! or, as `tests/in_process_exchange.rs` did, hand-built a [`Negotiation`] and
//! asserted against a session that had never been negotiated with anyone.
//!
//! ## The two handshakes are not the same shape
//!
//! Four revisions answer `initialize`, and its result carries the three fields
//! that matter: `protocolVersion`, `capabilities`, `serverInfo`.
//! `2026-07-28` (SEP-2575) has no `initialize` at all — its vendored schema has
//! no `InitializeRequest` and no `InitializeResult` — and a session is
//! established by `server/discover` instead, whose result names a *set* of
//! revisions rather than one, and carries no `serverInfo` at all.
//!
//! So "does this revision have a handshake" is the wrong question to ask a
//! client. [`handshake_defined_in`] answers it, and is still the right question
//! to ask about `initialize` specifically; what this module acts on is which of
//! the two a revision has.
//!
//! [`Negotiation`]: crate::domain::Negotiation

use serde_json::Value;

use crate::application::client::{ClientConfig, ClientTransport, TransportError};
use crate::domain::ir::{JsonObject, JsonObjectError};
use crate::domain::{
    CLIENT_CAPABILITY_WIRE, CapabilitySet, NegotiatedSession, NegotiationError, PeerInfo,
    ProtocolVersion, SERVER_CAPABILITY_WIRE, SupportedVersions, handshake_defined_in,
    negotiate_client,
};

/// The method that opens a `2026-07-28` session.
///
/// The one method in that revision that reports what the server can do, and so
/// the reason a client at that revision is not guessing its way to a profile.
pub const DISCOVER: &str = "server/discover";

/// The method that opens every other session this crate speaks.
pub const INITIALIZE: &str = "initialize";

/// The notification that ends a handshake, on the revisions that have one.
///
/// Absent from the `2026-07-28` lifecycle, whose schema has no
/// `InitializedNotification`, so this is never sent at that revision.
pub const INITIALIZED: &str = "notifications/initialized";

/// Who this client says it is.
///
/// Not optional on the wire: `InitializeRequest` marks `clientInfo` required,
/// with `name` and `version` both required inside it, so a client with no name
/// still has to send something. Reporting the real crate beats reporting
/// nothing — a server that logs what it connected to is then logging a true
/// thing, and a caller cannot make the field wrong.
///
/// A function rather than a `const` because [`PeerInfo`] holds two `String`s.
/// Two allocations once per handshake is not a cost worth a static to avoid.
fn self_info() -> PeerInfo {
    PeerInfo {
        name: env!("CARGO_PKG_NAME").to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// Why a session could not be established.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandshakeError {
    /// The client was built offering no revision.
    ///
    /// Separate from an unsupported *server* because the two need different
    /// fixes: one is a misconfigured client, the other is a peer this build
    /// cannot speak to.
    #[error("this client was offered no revision, so there is nothing to negotiate")]
    NothingOffered,

    /// The peer's answer is missing a field the schema marks required, or the
    /// field holds the wrong shape.
    ///
    /// Named by field so the reader knows which part of the peer to look at.
    /// "Malformed initialize result" sends them looking at the whole message.
    #[error("the {method} answer has no usable {field}: {detail}")]
    MalformedResult {
        /// Which handshake this was.
        method: &'static str,
        /// Which required field is wrong.
        field: &'static str,
        /// What was there instead.
        detail: String,
    },

    /// This build cannot speak any revision the peer offers.
    #[error(transparent)]
    Negotiation(#[from] NegotiationError),

    /// The request or the notification could not be delivered.
    #[error("the handshake did not complete: {0}")]
    Transport(#[from] TransportError),

    /// A params object this client assembled was not valid JSON.
    #[error("the {method} request could not be assembled: {source}")]
    Assembling {
        /// Which handshake this was.
        method: &'static str,
        /// The underlying parse failure.
        source: JsonObjectError,
    },
}

/// Establishes a session over `transport`.
///
/// `?Sized` because [`Client`](crate::application::client::Client) holds an `Arc<dyn ClientTransport>`, and a
/// caller who has one of those could not otherwise use the handshake that
/// produces the session the `Client` wants. Requiring a concrete transport
/// here would mean the caller keeps the concrete type alive solely to call
/// this, then boxes it immediately afterwards — a step with no purpose beyond
/// satisfying a bound.
///
/// Sends [`INITIALIZE`] on every revision that has it and `server/discover` on
/// `2026-07-28`, then — on the revisions that have one — the [`INITIALIZED`]
/// notification. The notification is not optional: a server that has not been
/// told the client is ready will not answer anything else, so a client that
/// skipped it would look connected and then hang on its first call.
///
/// # Errors
///
/// [`HandshakeError::Transport`] if the request or the notification could not be
/// delivered, [`HandshakeError::MalformedResult`] if a required field of the
/// answer is missing, and [`NegotiationError`] through
/// [`HandshakeError::Negotiation`] if the two sides have no revision in common.
pub async fn handshake(
    transport: &(impl ClientTransport + ?Sized),
    config: &ClientConfig,
) -> Result<NegotiatedSession, HandshakeError> {
    let (preferred, supported) = offer(config).ok_or(HandshakeError::NothingOffered)?;

    // A `2026-07-28` session is established by discovery rather than by an
    // `initialize` round trip, and its lifecycle has no `initialized`
    // notification to send at the end. Both facts are read off the vendored
    // schema rather than written here as a version comparison.
    if !handshake_defined_in(preferred) {
        let answer = transport.send(DISCOVER, &empty_params()?).await?;
        return discover_session(&answer, &supported, config);
    }

    let answer = transport
        .send(INITIALIZE, &initialize_params(preferred, config)?)
        .await?;
    let session = initialize_session(&answer, preferred, &supported, config)?;

    transport.notify(INITIALIZED, &empty_params()?).await?;
    Ok(session)
}

/// The revision this client prefers, and the set it can speak.
///
/// `None` for a client that offers nothing, which
/// [`ClientConfig::speaking`] already refuses to build — so this is a
/// belt-and-braces answer for a config assembled some other way.
///
/// [`ClientConfig::speaking`]: super::ClientConfig::speaking
#[must_use]
pub fn offer(config: &ClientConfig) -> Option<(ProtocolVersion, SupportedVersions)> {
    let supported = SupportedVersions::from_slice(config.versions()).ok()?;
    let preferred = *config.versions().first()?;
    Some((preferred, supported))
}

/// The `initialize` params: the offer, what this client can do, and who it is.
///
/// The three required fields of `InitializeRequest`. The capabilities are
/// rendered from the client's own set rather than passed in as a `Value`, so
/// there is no way to advertise a capability the profile behind it would
/// refuse.
///
/// # Errors
///
/// [`HandshakeError::Assembling`] if the params do not serialise, which for
/// this input cannot happen — a map of strings and booleans always does.
pub fn initialize_params(
    preferred: ProtocolVersion,
    config: &ClientConfig,
) -> Result<JsonObject, HandshakeError> {
    let mut info = serde_json::Map::new();
    info.insert("name".to_owned(), Value::from(self_info().name));
    info.insert("version".to_owned(), Value::from(self_info().version));

    let mut params = serde_json::Map::new();
    params.insert(
        "protocolVersion".to_owned(),
        Value::from(preferred.to_string()),
    );
    params.insert(
        "capabilities".to_owned(),
        Value::Object(client_capabilities(config)),
    );
    params.insert("clientInfo".to_owned(), Value::Object(info));

    to_params(INITIALIZE, &Value::Object(params))
}

/// The empty params a notification and a discovery request both carry.
fn empty_params() -> Result<JsonObject, HandshakeError> {
    to_params(DISCOVER, &Value::Object(serde_json::Map::new()))
}

/// Serialises a params object, naming the handshake it belongs to on failure.
fn to_params(method: &'static str, value: &Value) -> Result<JsonObject, HandshakeError> {
    JsonObject::new(value.to_string())
        .map_err(|source| HandshakeError::Assembling { method, source })
}

/// Renders the client's own capabilities as a `ClientCapabilities` object.
///
/// Walks [`CLIENT_CAPABILITY_WIRE`], the mirror of the table the server's
/// renderer reads, so a capability is either rendered under the key the schema
/// gives it or not rendered at all. Deriving the key from `Capability::as_str`
/// instead would be wrong for every nested capability: `roots.listChanged` is a
/// dot in a Rust name and two levels in JSON.
///
/// [`CLIENT_CAPABILITY_WIRE`]: crate::domain::CLIENT_CAPABILITY_WIRE
pub(super) fn client_capabilities(config: &ClientConfig) -> serde_json::Map<String, Value> {
    let mut root = serde_json::Map::new();
    for (capability, path) in CLIENT_CAPABILITY_WIRE {
        if config.capabilities().contains(*capability) {
            insert_path(&mut root, path);
        }
    }
    root
}

/// Asserts `path` on `root`, creating every object on the way.
///
/// A path of one key leaves an **empty object** rather than a boolean, because
/// a parent capability's presence is the claim and the schema gives it an
/// object: `ClientCapabilities.roots` is an object, so `"roots": {}` is the
/// wire form and `"roots": true` is not one any schema admits. A longer path
/// ends in `true`, because a sub-capability like `listChanged` genuinely is a
/// boolean.
fn insert_path(root: &mut serde_json::Map<String, serde_json::Value>, path: &[&str]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut cursor = root;
    for key in parents {
        let entry = cursor
            .entry((*key).to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let serde_json::Value::Object(parent) = entry else {
            return;
        };
        cursor = parent;
    }
    let leaf = if parents.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::Value::Bool(true)
    };
    cursor.insert((*last).to_string(), leaf);
}

/// Reads an `initialize` answer into a session.
///
/// The three required fields of `InitializeResult` and nothing else.
/// `instructions` is ignored rather than carried: it is a human-facing hint with
/// no client behaviour behind it, and a session that stored it would hand
/// callers a field nothing gates on.
///
/// # Errors
///
/// [`HandshakeError::MalformedResult`] naming the field that is wrong, and
/// [`NegotiationError::UnsupportedServerVersion`] when the answer is a revision
/// this build cannot speak.
pub fn initialize_session(
    answer: &JsonObject,
    preferred: ProtocolVersion,
    supported: &SupportedVersions,
    config: &ClientConfig,
) -> Result<NegotiatedSession, HandshakeError> {
    let result = parse(answer);
    let answered = required_version(&result, INITIALIZE, "protocolVersion")?;
    let server_caps = server_capabilities(&result, INITIALIZE)?;
    let server_info = peer_info(&result, "serverInfo");

    let version = negotiate_client(preferred, answered, supported)?;
    let session = NegotiatedSession::new(
        version,
        config.transport(),
        config.capabilities(),
        server_caps,
    )
    .with_client_info(self_info());
    Ok(match server_info {
        Some(info) => session.with_server_info(info),
        None => session,
    })
}

/// Reads a `server/discover` answer into a session.
///
/// Different from [`initialize_session`] by construction: the peer names a *set*
/// of revisions and the client picks, there is no `serverInfo` to read, and the
/// capabilities arrive under a key the older revisions do not use.
///
/// The pick is [`SupportedVersions::best_match`] — the newest revision both
/// sides speak — rather than the client's own preference. A client that picked
/// its own preference would ignore a server that had dropped that revision, and
/// would then be running on a version the server never offered.
///
/// # Errors
///
/// [`HandshakeError::MalformedResult`] when `supportedVersions` is absent or
/// holds nothing this build speaks.
pub fn discover_session(
    answer: &JsonObject,
    supported: &SupportedVersions,
    config: &ClientConfig,
) -> Result<NegotiatedSession, HandshakeError> {
    let result = parse(answer);
    let offered = required_version_set(&result)?;
    let server_caps = server_capabilities(&result, DISCOVER)?;

    let version = supported.best_match(&offered).ok_or_else(|| {
        HandshakeError::MalformedResult {
            method: DISCOVER,
            field: "supportedVersions",
            detail: format!(
                "it offers {offered:?}, none of which this client speaks (it speaks {supported})"
            ),
        }
    })?;

    Ok(NegotiatedSession::new(
        version,
        config.transport(),
        config.capabilities(),
        server_caps,
    )
    .with_client_info(self_info()))
}

/// Parses an answer, which the port hands over in its serialised form.
///
/// Infallible, and that is a fact about [`JsonObject`] rather than a guess:
/// the type validated the text as a well-formed JSON *object* on the way in, so
/// there is no text this can be handed that would not parse and nothing to
/// check for being an object. Re-checking anyway would be a second copy of a
/// guarantee that already exists, and the day someone relaxes `JsonObject` to
/// carry any JSON — which the IR has no reason to do, but might — a reader that
/// has forgotten the check would silently start reading an array as a result.
fn parse(answer: &JsonObject) -> Value {
    // The `unwrap`-free read: `from_str` on validated text cannot fail, and a
    // failure would mean `JsonObject` had stopped promising what its own docs
    // say. Reported rather than panicked on, because "this can never happen"
    // is exactly the kind of claim that deserves a path out.
    serde_json::from_str(answer.as_str()).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

/// Reads a required `protocolVersion` string.
fn required_version(
    result: &Value,
    method: &'static str,
    field: &'static str,
) -> Result<ProtocolVersion, HandshakeError> {
    let raw = result.get(field).and_then(Value::as_str).ok_or_else(|| {
        HandshakeError::MalformedResult {
            method,
            field,
            detail: describe(result.get(field)),
        }
    })?;
    raw.parse::<ProtocolVersion>()
        .map_err(|error| HandshakeError::MalformedResult {
            method,
            field,
            detail: format!("{raw:?} is not a protocol version this build knows: {error}"),
        })
}

/// Reads the required `supportedVersions` array of `server/discover`.
///
/// An entry this build has never heard of is **skipped** rather than refused,
/// because the field is a set: one unfamiliar revision does not make the others
/// unusable, and refusing the whole answer over it would mean a server could
/// not add a revision without breaking every client that had not heard of it
/// yet. The revision that is then missing is reported by
/// [`SupportedVersions::best_match`] returning `None`, which names the whole
/// set rather than the single entry that went unrecognised.
fn required_version_set(result: &Value) -> Result<Vec<ProtocolVersion>, HandshakeError> {
    let raw = result
        .get("supportedVersions")
        .and_then(Value::as_array)
        .ok_or_else(|| HandshakeError::MalformedResult {
            method: DISCOVER,
            field: "supportedVersions",
            detail: describe(result.get("supportedVersions")),
        })?;
    Ok(raw
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|text| text.parse::<ProtocolVersion>().ok())
        .collect())
}

/// Reads a `capabilities` object into a set.
///
/// Walks [`SERVER_CAPABILITY_WIRE`] rather than the keys actually present,
/// because the table knows which of those keys are capabilities and which are
/// sub-capabilities of one. A reader that treated every key as a capability
/// would report `listChanged` as one, and the profile built from that would
/// then be gating a feature on a key the peer never meant as a claim.
fn server_capabilities(
    result: &Value,
    method: &'static str,
) -> Result<CapabilitySet, HandshakeError> {
    let raw = result
        .get("capabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| HandshakeError::MalformedResult {
            method,
            field: "capabilities",
            detail: describe(result.get("capabilities")),
        })?;

    let mut set = CapabilitySet::empty();
    for (capability, path) in SERVER_CAPABILITY_WIRE {
        if read_path(raw, path) {
            set.insert(*capability);
        }
    }
    Ok(set)
}

/// Whether `path` is present in `object` as a capability claim.
///
/// A one-key path is satisfied by the object's own presence, which is the whole
/// signal for a parent capability. A longer path needs every segment present
/// and the last one to be `true`: a `tools` object with `listChanged: false` is
/// a server that has tools and will not announce changes, which is a different
/// session from one with no tools at all.
fn read_path(object: &serde_json::Map<String, Value>, path: &[&str]) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let mut cursor = object;
    for key in parents {
        let Some(next) = cursor.get(*key).and_then(Value::as_object) else {
            return false;
        };
        cursor = next;
    }
    match cursor.get(*last) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Object(_)) if parents.is_empty() => true,
        _ => false,
    }
}

/// Reads a `PeerInfo`, or `None` when the revision has no such field.
fn peer_info(result: &Value, field: &str) -> Option<PeerInfo> {
    let object = result.get(field)?.as_object()?;
    Some(PeerInfo {
        name: object.get("name")?.as_str()?.to_owned(),
        version: object.get("version")?.as_str()?.to_owned(),
    })
}

/// What was in a field that should have held something else.
fn describe(value: Option<&Value>) -> String {
    value.map_or_else(
        || "the field is absent".to_owned(),
        |value| format!("it holds {value}"),
    )
}

#[cfg(test)]
#[path = "handshake/tests.rs"]
mod tests;
