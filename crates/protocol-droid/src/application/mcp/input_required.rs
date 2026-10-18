//! SEP-2322 multi round-trip requests, introduced at **2026-07-28**.
//!
//! # What replaced server-initiated requests
//!
//! Through `2025-11-25` a tool that needed the user asked the client directly:
//! it sent `elicitation/create` down the response stream and waited for the
//! answer to come back on a separate request. `2026-07-28` has no
//! server-to-client requests at all — the schema's `ServerRequest` union is
//! gone — because a stateless server has nowhere to wait. Instead the tool
//! *answers* with an `InputRequiredResult` naming what it needs, and the client
//! retries the original request carrying `inputResponses` (and the server's
//! `requestState`, if it sent one).
//!
//! So the shape a handler writes is a result, not a call:
//!
//! - round one returns [`InputRequired`], built here;
//! - round two reads the answers off its
//!   [`RequestContext`](crate::application::server::RequestContext).
//!
//! # Why `requestState` is sealed by the library
//!
//! `requestState` is the server's own memory, held by the client between
//! rounds. A client that edits it can make a server believe it already asked a
//! question, or already got an answer, it never did. The SEP's scenario for
//! this is explicit: a tampered state is rejected. Sealing it here, under a key
//! the client never sees, means every handler gets that rejection without
//! writing it — the dispatcher refuses a state that does not open before the
//! handler runs.
//!
//! Sealed, not encrypted: the state is integrity-protected, and its content is
//! visible to the client. A handler that needs a secret in it must not put one
//! there; this is the same contract the SEP gives `requestState` itself.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::domain::ir::JsonObject;

/// The value `resultType` carries on an interim result.
pub const INPUT_REQUIRED: &str = "input_required";

/// The key a server seals `requestState` under.
///
/// Thirty-two bytes, the HMAC-SHA256 block-size recommendation's lower bound,
/// supplied by the caller rather than generated here: this layer may not draw
/// from a random source itself. The transports fill one from the OS CSPRNG
/// when the server was built without one.
#[derive(Clone)]
pub struct StateKey([u8; 32]);

impl StateKey {
    /// A key from raw bytes.
    ///
    /// For a deployment running several replicas behind one endpoint, every
    /// replica must be given the *same* key, or a retry routed to a different
    /// replica will find its state refused as tampered.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Seals `state` so that any change to it is detectable.
    ///
    /// The format is `<hex mac>.<state>`: the MAC is fixed-width, so the first
    /// `.` always ends it and a state containing dots of its own needs no
    /// escaping.
    #[must_use]
    pub fn seal(&self, state: &str) -> Option<String> {
        let mut mac = self.hmac()?;
        mac.update(state.as_bytes());
        let mac = hex(&mac.finalize().into_bytes());
        Some(format!("{mac}.{state}"))
    }

    /// The state a sealed value carries, or `None` if it was altered.
    ///
    /// The comparison is the MAC crate's own, which is constant-time; a
    /// byte-by-byte `==` would tell an attacker how many leading bytes of a
    /// forged MAC were right.
    #[must_use]
    pub fn open<'a>(&self, sealed: &'a str) -> Option<&'a str> {
        let (mac, state) = sealed.split_once('.')?;
        let expected = unhex(mac)?;
        let mut verifier = self.hmac()?;
        verifier.update(state.as_bytes());
        verifier.verify_slice(&expected).ok()?;
        Some(state)
    }

    /// The keyed MAC. HMAC accepts a key of any length, so this is `None`
    /// only if that stops being true of the crate behind it.
    fn hmac(&self) -> Option<Hmac<Sha256>> {
        <Hmac<Sha256> as KeyInit>::new_from_slice(&self.0).ok()
    }
}

impl core::fmt::Debug for StateKey {
    /// Never prints the key.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("StateKey(<redacted>)")
    }
}

fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let pair = core::str::from_utf8(pair).ok()?;
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}

/// An interim result: what the server needs before it can finish.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::mcp::input_required::InputRequired;
/// use protocol_droid::domain::ir::JsonObject;
///
/// let ask = JsonObject::new(
///     r#"{"message":"Name?","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}}}}"#,
/// )?;
/// let result = InputRequired::new().elicit("user_name", &ask).into_result();
/// assert!(result.as_str().contains(r#""resultType":"input_required""#));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct InputRequired {
    requests: serde_json::Map<String, serde_json::Value>,
    state: Option<String>,
}

impl InputRequired {
    /// An interim result asking for nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the client's user for input, under `key`.
    #[must_use]
    pub fn elicit(self, key: impl Into<String>, params: &JsonObject) -> Self {
        self.request(key, "elicitation/create", params)
    }

    /// Asks the client to sample from a model, under `key`.
    #[must_use]
    pub fn sample(self, key: impl Into<String>, params: &JsonObject) -> Self {
        self.request(key, "sampling/createMessage", params)
    }

    /// Asks the client for its roots, under `key`.
    #[must_use]
    pub fn list_roots(self, key: impl Into<String>) -> Self {
        self.request(key, "roots/list", &JsonObject::default())
    }

    /// Carries a sealed `requestState` the client will echo on its retry.
    ///
    /// Takes the *sealed* form — see
    /// [`RequestContext::seal_state`](crate::application::server::RequestContext::seal_state).
    #[must_use]
    pub fn with_state(mut self, sealed: impl Into<String>) -> Self {
        self.state = Some(sealed.into());
        self
    }

    fn request(mut self, key: impl Into<String>, method: &str, params: &JsonObject) -> Self {
        let params = serde_json::from_str(params.as_str())
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
        self.requests.insert(
            key.into(),
            serde_json::json!({ "method": method, "params": params }),
        );
        self
    }

    /// The result document.
    ///
    /// `resultType` is written here rather than left to the dispatcher's
    /// envelope, because it is what makes this an interim result at all: the
    /// envelope stamps `"complete"` on anything that does not already say
    /// otherwise.
    #[must_use]
    pub fn into_result(self) -> JsonObject {
        let mut result = serde_json::Map::new();
        result.insert("resultType".to_owned(), INPUT_REQUIRED.into());
        if !self.requests.is_empty() {
            result.insert(
                "inputRequests".to_owned(),
                serde_json::Value::Object(self.requests),
            );
        }
        if let Some(state) = self.state {
            result.insert("requestState".to_owned(), state.into());
        }
        JsonObject::from_value(serde_json::Value::Object(result)).unwrap_or_default()
    }
}

/// What a retry carried back: the client's answers and the server's state.
///
/// Built by the dispatcher, once the state has been opened, and handed to the
/// handler through its context. A first round has neither.
#[derive(Debug, Clone, Default)]
pub struct InputRound {
    pub(crate) responses: Option<serde_json::Map<String, serde_json::Value>>,
    pub(crate) state: Option<String>,
    pub(crate) key: Option<StateKey>,
}

impl InputRound {
    /// The client's answer under `key`, if it sent one.
    #[must_use]
    pub fn response(&self, key: &str) -> Option<JsonObject> {
        let value = self.responses.as_ref()?.get(key)?;
        JsonObject::from_value(value.clone()).ok()
    }

    /// Whether this is a retry carrying any answers at all.
    #[must_use]
    pub const fn has_responses(&self) -> bool {
        self.responses.is_some()
    }

    /// The state the server sealed on the previous round, already verified.
    #[must_use]
    pub fn state(&self) -> Option<&str> {
        self.state.as_deref()
    }

    /// Seals `state` for the next round, or `None` without a key.
    #[must_use]
    pub fn seal(&self, state: &str) -> Option<String> {
        self.key.as_ref().and_then(|key| key.seal(state))
    }
}

/// Whether a result is an interim one.
#[must_use]
pub fn is_input_required(result: &serde_json::Map<String, serde_json::Value>) -> bool {
    result.get("resultType").and_then(serde_json::Value::as_str) == Some(INPUT_REQUIRED)
}

/// The client capability each input request needs, by method.
///
/// The three the schema's `InputRequest` union admits; anything else is not an
/// input request at all and needs nothing, because it will not be sent.
#[must_use]
pub fn capability_for(method: &str) -> Option<&'static str> {
    match method {
        "sampling/createMessage" => Some("sampling"),
        "elicitation/create" => Some("elicitation"),
        "roots/list" => Some("roots"),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
