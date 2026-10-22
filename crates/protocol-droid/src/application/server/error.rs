//! The two error types the server API returns.
//!
//! They are not variants of one error, because they mean opposite things about
//! what the peer learns.
//!
//! [`GatedError`] is a refusal that never reaches the wire. The request was
//! well formed and the server simply cannot serve it the way the handler
//! imagined, so the failure stays on this side and the handler is told
//! directly. Sending a JSON-RPC error there teaches the peer something false
//! about the protocol.
//!
//! [`DispatchError`] is a failure while a method was being dispatched, and
//! most of its variants do reach the peer as a JSON-RPC error — the peer named
//! a tool that does not exist, or sent parameters the server could not read.
//! The peer is owed an answer in those cases, and which answer is what the
//! variants distinguish.

use crate::domain::ProtocolVersion;
use crate::domain::feature::Feature;
use crate::domain::idempotency::IdempotencyError;
use crate::domain::ir::{IrErrorCode, JsonObject};

/// Why a feature-specific call was refused.
///
/// Every variant here is a refusal the peer never sees. The distinction from a
/// JSON-RPC error matters: an error the peer sees is a protocol interaction,
/// and this crate is careful about when it sends one. A handler that asks for
/// a capability the session does not have is not a protocol error — the request
/// was well formed and the server simply cannot serve it the way the handler
/// imagined. Sending a JSON-RPC error there teaches the peer something false
/// about the protocol, so the failure stays on this side of the wire and the
/// handler is told directly.
///
/// That is the whole reason this type exists rather than reusing the
/// lower-time `LowerError`: one happens during a translation the peer asked
/// for, the other happens before anything is written at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatedError {
    /// The negotiated revision and the peer's declared capabilities do not
    /// include the feature the call needs.
    ///
    /// The negotiated version is carried rather than left to the caller to
    /// remember, because "elicitation is unavailable" is an answer nobody can
    /// act on without knowing what *was* negotiated.
    #[error("{feature} was not negotiated (session is {negotiated})")]
    FeatureUnavailable {
        /// The feature the call needed.
        feature: Feature,
        /// The revision this session actually settled on.
        negotiated: ProtocolVersion,
    },

    /// A progress report was attempted on a request that was never given a
    /// progress token.
    ///
    /// Separate from [`FeatureUnavailable`](Self::FeatureUnavailable) because
    /// the fix is different: progress is available on every revision, and the
    /// client is the one that declined to open a channel.
    #[error("this request has no progress token; the client did not open one")]
    NoProgressToken,

    /// A `requestState` was to be sealed and the server holds no key.
    ///
    /// A server built in-process with no key, serving through no transport
    /// that would have supplied one. Sealing under a placeholder would hand
    /// out a state any client could forge, which is worse than refusing.
    #[error("this server has no request-state key to seal with")]
    NoStateKey,

    /// The peer's answer to a server-initiated request did not arrive as JSON,
    /// or the exchange did not complete.
    ///
    /// The gate is not implicated — the call was permitted, and the failure is
    /// the peer's or the transport's. Kept here so a handler has one error
    /// type to match rather than a gate error and a transport error.
    #[error("the exchange did not complete: {reason}")]
    MalformedAnswer {
        /// What the transport reported.
        ///
        /// A `String` because the transport names its own failures and the
        /// application layer has no business enumerating them. The rule is that
        /// application code never names a _third-party_ error type; a
        /// transport's own description is data the adapter chose to hand over.
        reason: String,
    },
}

impl GatedError {
    /// The feature this refusal is about, when there is one.
    ///
    /// A progress-token refusal is about no feature, which is why this returns
    /// an `Option` rather than inventing one — a caller logging the answer
    /// wants to omit the field, not print a placeholder that reads as a claim.
    #[must_use]
    pub const fn feature(&self) -> Option<Feature> {
        match self {
            Self::FeatureUnavailable { feature, .. } => Some(*feature),
            Self::NoProgressToken | Self::NoStateKey | Self::MalformedAnswer { .. } => None,
        }
    }

    /// Whether nothing was written to the transport before this refusal.
    ///
    /// Always true, and that is the point: the gate runs before the exchange,
    /// so a refused call cannot have left a partial frame behind. The
    /// [`MalformedAnswer`](Self::MalformedAnswer) case is the exception in
    /// spirit — the request _was_ written, because the gate had passed and the
    /// failure came afterwards — so it reports false.
    #[must_use]
    pub const fn wrote_nothing(&self) -> bool {
        match self {
            Self::FeatureUnavailable { .. } | Self::NoProgressToken | Self::NoStateKey => true,
            Self::MalformedAnswer { .. } => false,
        }
    }
}

/// Why a tool call did not run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DispatchError {
    /// No tool of that name is registered.
    ///
    /// Deliberately the same answer for a tool that exists but is hidden from
    /// this session: the difference would let a client probe for the existence
    /// of a tool it may not see. The server still logs which it was.
    #[error("no such tool")]
    UnknownTool,

    /// The tool is registered, but the session did not negotiate the features
    /// it requires.
    #[error("the session did not negotiate what this tool requires")]
    NotCallable,

    /// No method of that name exists.
    ///
    /// Refused on the method, not on a tool, because the caller named a method
    /// and there is no tool to say anything about.
    #[error("no such method")]
    UnknownMethod,
    /// No resource of that URI is registered, or it is listed without a
    /// reader.
    ///
    /// Two causes share one answer because a peer cannot tell them apart: a
    /// listing is a promise, so a resource that appears in `resources/list`
    /// and then refuses to be read is the same failure as one that was never
    /// there, and naming which of the two happened tells the peer nothing it
    /// can act on.
    #[error("no such resource")]
    UnknownResource,
    /// No prompt of that name is registered, or it is listed without a getter.
    #[error("no such prompt")]
    UnknownPrompt,
    /// A registered reader or getter refused, naming why.
    ///
    /// Carries the handler's own message rather than a category, because the
    /// peer is a client and the only thing it can do with a category is show it
    /// to a user who would rather read what went wrong.
    #[error("the handler refused: {0}")]
    HandlerFailed(String),

    /// `tools/call` params were not the shape a call has to be: not an object,
    /// no string `name`, or an `arguments` that is not an object.
    ///
    /// A distinct variant from `UnknownTool` because a call with no `name` at
    /// all has not failed to find a tool — it never named one, and answering
    /// "no such tool" would send the caller looking for a tool that works.
    #[error("the call parameters are not a tool call")]
    MalformedParams,

    /// `initialize` params were not the shape the handshake has to be.
    ///
    /// Separate from [`MalformedParams`](Self::MalformedParams) because the
    /// two failures are not the same event: that one says a tool call named
    /// nothing, and this one says the one request that establishes a session
    /// arrived in a shape that would have to be guessed at. A peer sending it
    /// has a bug worth naming, not a tool to go looking for.
    ///
    /// Covers a body that is not an object, a missing or non-string
    /// `protocolVersion`, a string that is not a protocol version, a missing or
    /// non-object `capabilities`, and a `clientInfo` with no `name`.
    #[error("the initialize parameters are not a handshake")]
    MalformedInitialize,

    /// The session is on a revision this build has no wire types for.
    ///
    /// Distinct from a refused *method* ([`UnknownMethod`](Self::UnknownMethod))
    /// because nothing about the request was wrong: the method exists, the
    /// params parse, and the answer cannot be built because the revision the
    /// session settled on is one [`negotiate_server`](crate::domain::negotiate_server) would have to counter-
    /// offer. The session should never be in this state — it is reported
    /// rather than papered over, because the alternative is emitting a result
    /// whose shape the revision cannot express.
    #[error("the session is on a revision this build cannot serve")]
    UnnegotiableVersion,

    /// A SEP-2575 request arrived without the `_meta` the schema requires.
    ///
    /// `2026-07-28` makes `RequestParams._meta` required on every request and
    /// requires two keys in it — the revision and the client's capabilities —
    /// because there is no handshake to carry them and no session to remember
    /// them on. A request missing either is a well-formed message with a
    /// missing argument, which is `-32602` and not a transport refusal.
    ///
    /// Its own variant rather than a reuse of
    /// [`MalformedInitialize`](Self::MalformedInitialize) because the two are
    /// different failures with different fixes: one says the handshake arrived
    /// in a shape that would have to be guessed at, this says the shape is
    /// right and a required key is not there.
    #[error("the request `_meta` is missing a required key")]
    MalformedRequestMeta,

    /// A SEP-2575 request named a revision this build does not implement.
    ///
    /// Not [`MalformedParams`](Self::MalformedParams), and the difference is
    /// the whole point of the code the specification assigns: `-32022` with
    /// `data.supported` is what lets a client retry against a version this
    /// server does speak, where `-32602` would send it looking for a bug in
    /// its own request.
    #[error("this server does not implement the requested protocol version")]
    UnsupportedVersion {
        /// The revision the client asked for, as it wrote it.
        requested: String,
        /// Every revision this build speaks, for the client to choose from.
        supported: Vec<String>,
    },

    /// A second request arrived with an id this session is still working on.
    ///
    /// The JSON-RPC reading, and the reason it is an error rather than a
    /// queue: the first request is a real, running, side-effecting call, and
    /// the second carries no way to say what should happen to it. Running the
    /// tool twice is the answer a retry gets by accident, and this is the
    /// answer it gets on purpose. A client that wants the first result waits
    /// for it and retries later — at which point the id is `Completed` and
    /// comes straight back.
    #[error("request {} is already in flight on this session", .0)]
    DuplicateInFlight(String),

    /// Serving the request needs client capabilities the request did not
    /// declare (SEP-2575, `-32021`).
    ///
    /// Raised when a handler answers with an `InputRequiredResult` asking for
    /// something the client said it cannot do: sending it anyway would ask a
    /// client to sample or elicit after it declined to, and the specification
    /// is explicit that a server "MUST NOT rely on capabilities the client has
    /// not declared".
    #[error("the request needs client capabilities it did not declare")]
    MissingClientCapability {
        /// The capability names, as `ClientCapabilities` keys.
        required: Vec<&'static str>,
    },

    /// A SEP-2322 `requestState` did not verify.
    ///
    /// `-32602`, because the state is a request parameter and this one is not a
    /// value this server issued. Refused before the handler runs, so no
    /// handler can be made to act on a state a client wrote.
    #[error("requestState failed its integrity check")]
    TamperedRequestState,

    /// The store holding recorded results could not be reached.
    ///
    /// Reported rather than swallowed because the alternative is the one thing
    /// this whole path exists to prevent: a store that errors, a caller that
    /// treats the error as "no claim", and a tool that runs a second time.
    /// Failing the call is the only answer that does not charge a client twice
    /// behind its back.
    #[error("recorded results are unavailable: {0}")]
    IdempotencyUnavailable(String),
}

impl DispatchError {
    /// The JSON-RPC code this refusal is reported as.
    ///
    /// Lives here rather than in a transport adapter because it is a property
    /// of the error, not of the way it is written down. It sat in the stdio
    /// adapter until the HTTP router needed the same mapping, and the two
    /// options at that point were both wrong: copy it and let the copies drift
    /// apart, or reach into another adapter and break the layering.
    ///
    /// Every refusal gets an answer. The alternative is to drop it, which
    /// leaves the peer waiting on a response that is never coming — and a
    /// server that hangs is worse than one that says no. The codes are the
    /// closest the standard offers: a method nobody serves is `-32601`, a call
    /// that does not name something callable is `-32602`, and a session that
    /// did not negotiate a feature — or one settled on a revision this build
    /// cannot serve — is a server-side condition rather than either.
    ///
    /// A malformed `initialize` is `-32602` like any other malformed params:
    /// the peer sent a request whose body it could not have meant, which is the
    /// same condition whatever the method was.
    ///
    /// # Examples
    ///
    /// ```
    /// # use protocol_droid::application::server::DispatchError;
    /// # use protocol_droid::domain::ir::IrErrorCode;
    /// assert_eq!(
    ///     DispatchError::UnknownMethod.jsonrpc_code(),
    ///     IrErrorCode::MethodNotFound,
    /// );
    /// assert_eq!(
    ///     DispatchError::MalformedParams.jsonrpc_code(),
    ///     IrErrorCode::InvalidParams,
    /// );
    /// ```
    #[must_use]
    pub const fn jsonrpc_code(&self) -> IrErrorCode {
        match self {
            Self::UnknownMethod => IrErrorCode::MethodNotFound,
            // A URI or a prompt name that is not registered is the same
            // failure as a tool name that is not, and answers the same way. The
            // specification is explicit that an unknown tool is reported as an
            // error rather than an empty result, and a client cannot act on a
            // distinction between "no such thing" and "this thing is empty".
            Self::MalformedParams
            | Self::MalformedInitialize
            | Self::UnknownTool
            | Self::UnknownResource
            | Self::UnknownPrompt
            | Self::MalformedRequestMeta
            | Self::TamperedRequestState => IrErrorCode::InvalidParams,
            Self::MissingClientCapability { .. } => IrErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY,
            // A request that names an id this session is already working on is
            // a frame that violates JSON-RPC, so it takes the code for a frame
            // the specification does not permit — not the code for a tool it
            // cannot run, which is a different failure with a different fix.
            Self::DuplicateInFlight(_) => IrErrorCode::InvalidRequest,
            // A store that cannot be reached is this server's problem, whatever
            // the peer sent, so it is a server-side code. Reporting it as the
            // caller's fault would send a client looking for a bug in its own
            // request when the recording of results is what failed.
            Self::NotCallable | Self::UnnegotiableVersion | Self::IdempotencyUnavailable(_) => {
                IrErrorCode::InternalError
            }
            // SEP-2575 assigns this its own code precisely so a client can
            // distinguish "you asked for something real that I do not speak"
            // from "your request was wrong", and act on the first by retrying.
            Self::UnsupportedVersion { .. } => IrErrorCode::UNSUPPORTED_PROTOCOL_VERSION,
            // A handler that refused is this server's problem whatever the peer
            // sent. The peer asked for a resource or a prompt, named it
            // correctly, and the answer is that the backing store is
            // unreachable — reporting that as the caller's fault would send a
            // client hunting in its own request for something it got right.
            Self::HandlerFailed(_) => IrErrorCode::InternalError,
        }
    }

    /// The `data` member this refusal's code requires, if it has one.
    ///
    /// `-32022` carries the revision the client asked for and every revision
    /// this build speaks; `-32021` carries `requiredCapabilities` as a
    /// `ClientCapabilities` object keyed by what is missing — not an array of
    /// names, because the schema types it as the object a client declares.
    /// Every other refusal carries none. One definition, so stdio and HTTP
    /// cannot disagree about it.
    #[must_use]
    pub fn wire_data(&self) -> Option<JsonObject> {
        let data = match self {
            Self::UnsupportedVersion {
                requested,
                supported,
            } => serde_json::json!({ "requested": requested, "supported": supported }),
            Self::MissingClientCapability { required } => {
                let capabilities: serde_json::Map<String, serde_json::Value> = required
                    .iter()
                    .map(|name| {
                        (
                            (*name).to_owned(),
                            serde_json::Value::Object(serde_json::Map::new()),
                        )
                    })
                    .collect();
                serde_json::json!({ "requiredCapabilities": capabilities })
            }
            _ => return None,
        };
        JsonObject::from_value(data).ok()
    }

    /// The code and message pair a transport writes to the wire.
    ///
    /// The message is [`thiserror`]'s own for the variant, never the method
    /// name the peer chose: a method name is a JSON string a peer controls, and
    /// JSON permits a newline inside one, so echoing it into a log line would
    /// let a peer close that line and open one of its own.
    #[must_use]
    pub fn as_wire(&self) -> (IrErrorCode, String) {
        (self.jsonrpc_code(), self.to_string())
    }
}

impl From<crate::application::server::result::OutcomeError> for DispatchError {
    /// A reader or getter that could not produce a document failed.
    ///
    /// Every variant becomes [`DispatchError::HandlerFailed`] carrying the
    /// outcome's own message. None of them is a bad request — the peer named a
    /// resource or a prompt that exists and sent well-formed params — so
    /// reporting any of them as `InvalidParams` would send a client looking in
    /// its own request for something it got right.
    fn from(error: crate::application::server::result::OutcomeError) -> Self {
        Self::HandlerFailed(error.to_string())
    }
}

impl From<IdempotencyError> for DispatchError {
    /// Keeps a poisoned or absent store from reading as an absent claim.
    fn from(error: IdempotencyError) -> Self {
        Self::IdempotencyUnavailable(error.to_string())
    }
}
