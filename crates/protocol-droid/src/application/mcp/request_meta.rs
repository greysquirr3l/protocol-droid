//! The per-request `_meta` block SEP-2575 introduced at **2026-07-28**.
//!
//! # Why the handshake moved into `_meta`
//!
//! Through `2025-11-25` a client's capabilities and revision arrive once, in
//! `initialize`, and every request after that rides the session they opened.
//! `2026-07-28` has no handshake: the schema makes `RequestParams._meta`
//! required on every request, and requires two keys in it —
//! `io.modelcontextprotocol/protocolVersion` and
//! `io.modelcontextprotocol/clientCapabilities`. A stateless peer repeats
//! them per request because there is no session to remember them on.
//!
//! The spec is explicit that the server must not carry them forward:
//!
//! > Capabilities are declared per-request rather than once at initialization;
//! > Servers MUST NOT infer capabilities from prior requests.
//!
//! That sentence is the reason this is a value object parsed from one body
//! rather than a field on the session: reading a request against the previous
//! request's capabilities is exactly the inference the SEP forbids, and a
//! design where that is reachable is a design where it will happen.
//!
//! # Why the parser lives beside `initialize`
//!
//! [`initialize::read_offer`] reads the same two facts off a different member
//! of a different message, and both go through the same
//! [`client_capabilities`] table. A second copy of that table would be a
//! second definition of what a client is allowed to say.
//!
//! [`client_capabilities`]: crate::application::mcp::initialize::read_offer

use crate::domain::ir::{JsonObject, LogLevel};
use crate::domain::session::ProgressToken;
use crate::domain::{CapabilitySet, ProtocolVersion};

/// The key naming the revision this request is written against.
pub const PROTOCOL_VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";

/// The key naming what the client supports, for this request.
pub const CLIENT_CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";

/// The key naming the client software, which the SEP makes a SHOULD.
pub const CLIENT_INFO_KEY: &str = "io.modelcontextprotocol/clientInfo";

/// The key opting a request in to `notifications/message` at a level.
///
/// SEP-2575 replaced `logging/setLevel` with it: a request that omits the key
/// receives no log records at all.
pub const LOG_LEVEL_KEY: &str = "io.modelcontextprotocol/logLevel";

/// What one request said about itself.
///
/// `capabilities` and `version` are not optional because the schema does not
/// make them so: both are in `RequestMetaObject`'s `required`. A request that
/// omits either is refused by [`RequestMeta::read`] rather than served with a
/// default, because a default here would be the server inventing a
/// capability the client never claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestMeta {
    /// The revision this request is written against.
    pub version: ProtocolVersion,
    /// What the client supports, read through the shared capability tables.
    pub capabilities: CapabilitySet,
    /// The client's self-reported name, when it sent one.
    ///
    /// A SHOULD in the SEP and self-reported by the definition: nothing in
    /// this crate's answers depends on it, and a server that trusted it would
    /// be trusting a string the schema calls unverified.
    pub client_name: Option<String>,
    /// The level this request opted in to log records at, or `None` for none.
    pub log_level: Option<LogLevel>,
}

/// Why a `_meta` block could not be read.
///
/// Split by cause because the three faults have three different answers, and
/// one error type would force every caller to re-derive which it had:
///
/// - [`Absent`](Self::Absent) is a pre-2026-07-28 request, or one from a
///   client that negotiated an older revision. It is not a fault at all.
/// - [`MissingVersion`](Self::MissingVersion) and
///   [`MissingCapabilities`](Self::MissingCapabilities) are SEP-2575's own
///   `-32602 Invalid params` cases, and the suite checks them separately.
/// - [`MalformedVersion`](Self::MalformedVersion) is the same fault with a
///   different cause: the key was there and its value was not a revision. The
///   spec answers that with
///   [`UnsupportedProtocolVersionError`](Self::MalformedVersion) — an
///   *unsupported version*, not a malformed request — so it cannot collapse
///   into the missing case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RequestMetaError {
    /// No `_meta` at all, which is the shape of every pre-2026-07-28 request.
    #[error("the request carries no `_meta` block")]
    Absent,

    /// `_meta` is present and is not a JSON object.
    #[error("`_meta` is not a JSON object")]
    Malformed,

    /// `_meta` names no `io.modelcontextprotocol/protocolVersion`.
    #[error("`_meta` is missing `{key}`", key = PROTOCOL_VERSION_KEY)]
    MissingVersion,

    /// The named version is not one this crate knows.
    #[error("`{PROTOCOL_VERSION_KEY}` is not a protocol version this build implements")]
    MalformedVersion,

    /// `_meta` names no `io.modelcontextprotocol/clientCapabilities`.
    #[error("`_meta` is missing `{key}`", key = CLIENT_CAPABILITIES_KEY)]
    MissingCapabilities,

    /// `io.modelcontextprotocol/logLevel` is present and is not a level the
    /// specification names.
    #[error("`{LOG_LEVEL_KEY}` is not a logging level")]
    MalformedLogLevel,

    /// The capabilities value is present and is not a JSON object.
    #[error("`{CLIENT_CAPABILITIES_KEY}` is not a JSON object")]
    MalformedCapabilities,
}

impl RequestMeta {
    /// Reads the block off a request's `params`.
    ///
    /// # Errors
    ///
    /// [`RequestMetaError`] for each of the faults above, distinguished as
    /// documented on the type.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::application::mcp::request_meta::{RequestMeta, RequestMetaError};
    /// use protocol_droid::domain::ir::JsonObject;
    /// use protocol_droid::domain::{Capability, ProtocolVersion};
    ///
    /// // One line, because the keys are long and rustfmt would otherwise
    /// // break the literal across lines — and `JsonObject` validates on
    /// // construction, so a newline inside the JSON is a parse error.
    /// let params = JsonObject::new(
    ///     r#"{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{"roots":{}},"io.modelcontextprotocol/clientInfo":{"name":"probe","version":"1"}}}"#,
    /// )?;
    ///
    /// let meta = RequestMeta::read(&params)?;
    /// assert_eq!(meta.version, ProtocolVersion::V2026_07_28);
    /// assert!(meta.capabilities.contains(Capability::Roots));
    /// assert_eq!(meta.client_name.as_deref(), Some("probe"));
    ///
    /// // A request with no `_meta` is an older revision, not a fault.
    /// let older = JsonObject::new(r#"{"method":"tools/list"}"#)?;
    /// assert_eq!(RequestMeta::read(&older).err(), Some(RequestMetaError::Absent));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read(params: &JsonObject) -> Result<Self, RequestMetaError> {
        let parsed: serde_json::Value =
            serde_json::from_str(params.as_str()).map_err(|_| RequestMetaError::Malformed)?;
        let object = parsed.as_object().ok_or(RequestMetaError::Malformed)?;
        let meta = object
            .get("_meta")
            .ok_or(RequestMetaError::Absent)?
            .as_object()
            .ok_or(RequestMetaError::Malformed)?;

        let version = meta
            .get(PROTOCOL_VERSION_KEY)
            .ok_or(RequestMetaError::MissingVersion)?
            .as_str()
            .ok_or(RequestMetaError::MalformedVersion)?
            .parse::<ProtocolVersion>()
            .map_err(|_| RequestMetaError::MalformedVersion)?;

        let capabilities = meta
            .get(CLIENT_CAPABILITIES_KEY)
            .ok_or(RequestMetaError::MissingCapabilities)?
            .as_object()
            .ok_or(RequestMetaError::MalformedCapabilities)?;

        let log_level = match meta.get(LOG_LEVEL_KEY) {
            None => None,
            Some(level) => Some(
                level
                    .as_str()
                    .and_then(|text| text.parse::<LogLevel>().ok())
                    .ok_or(RequestMetaError::MalformedLogLevel)?,
            ),
        };

        Ok(Self {
            version,
            log_level,
            capabilities: client_capabilities(capabilities),
            client_name: meta
                .get(CLIENT_INFO_KEY)
                .and_then(serde_json::Value::as_object)
                .and_then(|info| info.get("name"))
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned),
        })
    }
}

/// The revision a request's `_meta` names, exactly as the client wrote it.
///
/// Separate from [`RequestMeta::read`] because the two callers that need it
/// need it *when that read fails*: a version this build cannot parse is still
/// a version the client asked for, and SEP-2575 answers it with
/// `UnsupportedProtocolVersionError` echoing `data.requested` — which has to be
/// the client's own spelling. The HTTP transport also compares it byte for
/// byte with the `MCP-Protocol-Version` header, which the schema says it MUST
/// match.
#[must_use]
pub fn version_text(params: &JsonObject) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(params.as_str()).ok()?;
    parsed
        .get("_meta")?
        .get(PROTOCOL_VERSION_KEY)?
        .as_str()
        .map(ToOwned::to_owned)
}

/// The progress token a request opened, if it opened one.
///
/// `params._meta.progressToken` on every revision that has `_meta`, and the
/// same member on `2024-11-05` and `2025-03-26`, whose schemas spell the
/// request-level `_meta` the same way. Optional in the strongest sense: a
/// request without one is valid and simply gets no progress.
#[must_use]
pub fn progress_token(params: &JsonObject) -> Option<ProgressToken> {
    let parsed: serde_json::Value = serde_json::from_str(params.as_str()).ok()?;
    ProgressToken::from_json(parsed.get("_meta")?.get("progressToken")?)
}

/// Reads a `ClientCapabilities` object as a [`CapabilitySet`].
///
/// The same table `read_offer` uses, reached through the same function: the
/// two messages say the same thing about the client in different members, and
/// a second table here would be a second answer to "what may a client say".
fn client_capabilities(object: &serde_json::Map<String, serde_json::Value>) -> CapabilitySet {
    crate::application::mcp::initialize::client_capabilities(object)
}

/// Whether this body is a SEP-2575 stateless request.
///
/// The cheapest possible answer to the question the router asks, and it is
/// deliberately cheap: this runs on every sessionless POST, and a full
/// [`RequestMeta::read`] would parse the whole body only to be discarded by a
/// caller that turns out to have a session header after all.
///
/// The presence of the version key *is* the test, because it is the only
/// member that can make a request self-describing: capabilities alone would
/// not say which revision to gate the answer on.
#[must_use]
pub fn declares_version(params: &serde_json::Value) -> bool {
    params
        .get("_meta")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|meta| meta.contains_key(PROTOCOL_VERSION_KEY))
}

#[cfg(test)]
mod tests {
    use crate::application::mcp::request_meta::{RequestMeta, RequestMetaError};
    use crate::domain::ir::JsonObject;
    use crate::domain::{Capability, CapabilitySet, ProtocolVersion};

    /// What a test here can fail with.
    #[derive(Debug)]
    enum Failure {
        /// The test's own complaint.
        Message(String),
        /// A fixture was not a JSON object.
        Fixture(crate::domain::ir::JsonObjectError),
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Message(text) => f.write_str(text),
                Self::Fixture(error) => write!(f, "bad fixture: {error}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<String> for Failure {
        fn from(text: String) -> Self {
            Self::Message(text)
        }
    }

    impl From<crate::domain::ir::JsonObjectError> for Failure {
        fn from(error: crate::domain::ir::JsonObjectError) -> Self {
            Self::Fixture(error)
        }
    }

    impl From<RequestMetaError> for Failure {
        fn from(error: RequestMetaError) -> Self {
            Self::Message(error.to_string())
        }
    }

    type TestError = Failure;

    /// Both required keys, read back as the revision and the capability set.
    ///
    /// Asserting on the whole `Result` rather than on a parsed value followed
    /// by an unwrap: on failure this prints the reader's own account of what
    /// it refused, which is what a failing schema change wants.
    #[test]
    fn both_required_keys_are_read() -> Result<(), TestError> {
        let params = JsonObject::new(
            r#"{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{"sampling":{},"roots":{}}}}"#,
        )?;
        let meta = RequestMeta::read(&params)?;

        assert_eq!(meta.version, ProtocolVersion::V2026_07_28);
        assert!(meta.capabilities.contains(Capability::Sampling));
        assert!(meta.capabilities.contains(Capability::Roots));
        assert_eq!(meta.client_name, None);
        Ok(())
    }

    /// An unrecognised capability is ignored, not refused.
    ///
    /// The same extensibility rule `read_offer` follows: a client built
    /// against a later revision names capabilities this build has never heard
    /// of, and refusing the request over one would make every future client
    /// unconnectable.
    #[test]
    fn an_unknown_capability_is_ignored() -> Result<(), TestError> {
        let params = JsonObject::new(
            r#"{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{"somethingNew":{}}}}"#,
        )?;
        let meta = RequestMeta::read(&params)?;

        assert_eq!(meta.capabilities, CapabilitySet::empty());
        Ok(())
    }

    /// Each missing key is its own fault, because each is its own `-32602`.
    #[test]
    fn each_missing_required_key_is_distinguished() -> Result<(), TestError> {
        let no_meta = JsonObject::new("{}")?;
        assert_eq!(
            RequestMeta::read(&no_meta).err(),
            Some(RequestMetaError::Absent),
            "a request with no _meta is an older revision, not a fault",
        );

        let no_version =
            JsonObject::new(r#"{"_meta":{"io.modelcontextprotocol/clientCapabilities":{}}}"#)?;
        assert_eq!(
            RequestMeta::read(&no_version).err(),
            Some(RequestMetaError::MissingVersion),
        );

        let no_capabilities = JsonObject::new(
            r#"{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}"#,
        )?;
        assert_eq!(
            RequestMeta::read(&no_capabilities).err(),
            Some(RequestMetaError::MissingCapabilities),
        );

        let bad_version = JsonObject::new(
            r#"{"_meta":{"io.modelcontextprotocol/protocolVersion":"not-a-version","io.modelcontextprotocol/clientCapabilities":{}}}"#,
        )?;
        assert_eq!(
            RequestMeta::read(&bad_version).err(),
            Some(RequestMetaError::MalformedVersion),
            "a named-but-unknown version is an unsupported version, not a missing key",
        );

        let bad_capabilities = JsonObject::new(
            r#"{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":"yes"}}"#,
        )?;
        assert_eq!(
            RequestMeta::read(&bad_capabilities).err(),
            Some(RequestMetaError::MalformedCapabilities),
        );
        Ok(())
    }

    /// `clientInfo` is a SHOULD: absent is fine, present is read, malformed is
    /// ignored rather than refused.
    ///
    /// The last one matters: nothing this crate answers depends on the name,
    /// so refusing a request over a `clientInfo` that is not an object would
    /// be stricter than the spec for a field the spec calls unverified.
    #[test]
    fn client_info_is_read_when_present_and_ignored_when_not() -> Result<(), TestError> {
        let named = JsonObject::new(
            r#"{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"probe","version":"1"}}}"#,
        )?;
        assert_eq!(
            RequestMeta::read(&named)?.client_name.as_deref(),
            Some("probe"),
        );

        let malformed = JsonObject::new(
            r#"{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":"probe"}}"#,
        )?;
        assert_eq!(RequestMeta::read(&malformed)?.client_name, None);
        Ok(())
    }
}
