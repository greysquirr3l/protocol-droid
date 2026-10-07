//! What a handler is given, and the gate in front of it.
//!
//! # The rule this module exists to enforce
//!
//! A feature-specific call consults the session's negotiated
//! [`FeatureProfile`] *before* anything is written. Not after, and not "write
//! and let the peer cope" — before. A server that emits
//! `elicitation/create` to a `2025-03-26` peer has told it something the
//! protocol does not allow, and no error afterwards repairs that.
//!
//! The testable form of that rule is [`GatedError::wrote_nothing`]: every
//! refusal in this module either wrote nothing at all, or is explicitly marked
//! as the one case that got far enough to write a request and then failed on
//! the answer. The two halves are not the same and the type says so.
//!
//! # Why there are two types and not one
//!
//! [`PeerSession`] is everything about the _connection_: the revision it
//! settled on, the profile that fell out of that, and the two ports a handler
//! needs to talk back. [`RequestContext`] is everything about the _request_
//! being served, and holds a reference to a session.
//!
//! The split is not tidiness. A profile and a transport are fixed when the
//! connection negotiates and shared by every request on it; a request id, a
//! cancellation token and a progress token differ on each. Collapsing them
//! produces a constructor with eight parameters, five of which every caller has
//! to invent for every request, and nothing stops a caller from taking them
//! from the wrong request.

use std::sync::Arc;

use crate::application::server::error::GatedError;
use crate::application::session::DynFrameSink;
use crate::domain::feature::{Feature, FeatureProfile};
use crate::domain::ir::JsonObject;
use crate::domain::registry::AuthContext;
use crate::domain::session::{CancellationToken, ProgressToken, SessionId};
use crate::domain::{ProtocolVersion, RequestId, TransportKind};

/// Why a server-initiated request could not complete.
///
/// Distinct from [`GatedError`]: the gate already passed by the time an
/// exchange runs, so this is the transport or the peer failing rather than the
/// capability being absent. They are separate types because a handler that
/// sees one should retry and a handler that sees the other should not.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the exchange did not complete: {reason}")]
pub struct ExchangeError {
    /// What went wrong, in the adapter's words.
    ///
    /// A `String` because the transport names its own failures (a closed
    /// stream, a cancelled request, a timeout) and the application layer has no
    /// business enumerating them. The rule is that application code never names
    /// a _third-party_ error type; a transport's own description is data the
    /// adapter chose to hand over, not a type this signature commits to.
    reason: String,
}

impl ExchangeError {
    /// Builds an exchange failure from whatever the transport reported.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// A round trip to the peer that a server-initiated request needs.
///
/// The port belongs to the consumer. It is declared here rather than in an
/// adapter because the shape of what a server needs to ask a peer is a decision
/// this module makes: a method name, a params object, and an answer back. A
/// transport implements it; nothing in `adapters/` defines it.
///
/// Not object-safe by design — it is an RPITIT `async fn`, so a caller that
/// knows its concrete exchange gets it inlined with no allocation. Callers that
/// genuinely need a boxed one get [`DynExchange`].
pub trait CallbackExchange {
    /// Sends `method` to the peer and returns its answer.
    ///
    /// The implementation owns framing: it knows the negotiated revision, it
    /// knows the method's wire name, and it parses the reply. Nothing above
    /// this trait ever sees a byte.
    ///
    /// # Errors
    ///
    /// [`ExchangeError`] when the request could not be delivered or the peer
    /// did not answer. Whether a non-answer is an error or a hang is the
    /// adapter's decision, and a server that waits forever is a server whose
    /// connection never closes.
    fn exchange(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> impl Future<Output = Result<JsonObject, ExchangeError>> + Send;
}

/// The boxed spelling of [`CallbackExchange`], for a caller holding a transport
/// of unknown concrete type.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use protocol_droid::application::server::context::{DynExchange, ExchangeError};
/// use protocol_droid::domain::ir::JsonObject;
///
/// let exchange: Arc<dyn DynExchange> = Arc::new(MyExchange);
/// // ...handed to a PeerSession, which calls it through the trait object.
/// # struct MyExchange;
/// # impl DynExchange for MyExchange {
/// #     fn exchange_boxed<'a>(&'a self, method: &'a str, params: &'a JsonObject)
/// #         -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, ExchangeError>> + Send + 'a>>
/// #     { Box::pin(async move { let _ = (method, params); Err(ExchangeError::new("unimplemented")) }) }
/// # }
/// ```
pub trait DynExchange: Send + Sync {
    /// The boxed form of [`CallbackExchange::exchange`].
    ///
    /// Written out rather than derived, for the same reason `DynFrameSink` is:
    /// the box belongs to callers that need dynamic dispatch, not to the ones
    /// that do not.
    fn exchange_boxed<'a>(
        &'a self,
        method: &'a str,
        params: &'a JsonObject,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, ExchangeError>> + Send + 'a>>;
}

impl<T: CallbackExchange + Send + Sync> DynExchange for T {
    fn exchange_boxed<'a>(
        &'a self,
        method: &'a str,
        params: &'a JsonObject,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, ExchangeError>> + Send + 'a>>
    {
        Box::pin(self.exchange(method, params))
    }
}

/// The negotiated connection a handler is serving.
///
/// Cheap to clone: a `Copy` revision, a `Copy` profile, a `Copy` session id and
/// two `Arc`s. A handler may hand one to a spawned task without the ceremony a
/// deep type would need.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use protocol_droid::application::server::context::{
///     CallbackExchange, ExchangeError, PeerSession,
/// };
/// use protocol_droid::domain::feature::{Feature, FeatureProfile};
/// use protocol_droid::domain::ir::JsonObject;
/// use protocol_droid::domain::session::SessionId;
/// use protocol_droid::domain::{ProtocolVersion, TransportKind};
///
/// struct NoExchange;
/// # impl CallbackExchange for NoExchange {
/// #     async fn exchange(&self, _: &str, _: &JsonObject) -> Result<JsonObject, ExchangeError> {
/// #         Err(ExchangeError::new("no peer"))
/// #     }
/// # }
/// # struct NullSink;
/// # impl protocol_droid::application::session::FrameSink for NullSink {
/// #     async fn send(&self, _: &str) -> Result<(), protocol_droid::application::session::SinkError> { Ok(()) }
/// # }
///
/// let peer = PeerSession::new(
///     ProtocolVersion::V2025_03_26,
///     FeatureProfile::empty().with(Feature::Sampling),
///     SessionId::new(1),
///     TransportKind::Stdio,
///     Arc::new(NoExchange),
///     Arc::new(NullSink),
/// );
///
/// // Elicitation was never negotiated; sampling was. A handler that asked the
/// // question up front gets an answer rather than a frame the peer may reject.
/// assert!(!peer.allows(Feature::Elicitation));
/// assert!(peer.allows(Feature::Sampling));
/// ```
#[derive(Clone)]
pub struct PeerSession {
    negotiated: ProtocolVersion,
    profile: FeatureProfile,
    session: SessionId,
    transport: TransportKind,
    auth: AuthContext,
    exchange: Arc<dyn DynExchange>,
    sink: Arc<dyn DynFrameSink>,
}

impl core::fmt::Debug for PeerSession {
    /// Prints the negotiation, and deliberately not the ports.
    ///
    /// A sink holds a channel and an exchange holds a socket; neither is
    /// readable and both are large, so the derived form would print
    /// unreadable noise. The two fields a post-mortem wants are the revision
    /// and how much the session can do.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PeerSession")
            .field("negotiated", &self.negotiated)
            .field("session", &self.session)
            .field("principal", &self.auth.principal())
            .field("feature_count", &self.profile.len())
            .finish_non_exhaustive()
    }
}

impl PeerSession {
    /// Records what a connection negotiated, and where its frames go.
    ///
    /// The caller is [`AuthContext::anonymous`], which holds no scopes. That
    /// is a default-deny default and not a convenience: a primitive requiring
    /// a scope is invisible to an anonymous session, and one requiring none is
    /// visible to everybody. [`with_auth`](Self::with_auth) is how a real
    /// principal arrives, so a transport that never calls it serves exactly
    /// the primitives nobody had to authorise.
    #[must_use]
    pub fn new(
        negotiated: ProtocolVersion,
        profile: FeatureProfile,
        session: SessionId,
        transport: TransportKind,
        exchange: Arc<dyn DynExchange>,
        sink: Arc<dyn DynFrameSink>,
    ) -> Self {
        Self {
            negotiated,
            profile,
            session,
            transport,
            auth: AuthContext::anonymous(),
            exchange,
            sink,
        }
    }

    /// The transport this session runs over.
    ///
    /// Part of the session rather than of the caller because a profile is not
    /// resolvable without one: [`resolve`] consults `transport_allowed` for
    /// every transport-gated feature row, so a handshake that needed the
    /// negotiated profile had nowhere to get the transport from until this
    /// carried it.
    #[must_use]
    pub const fn transport(&self) -> TransportKind {
        self.transport
    }

    /// The caller, for the visibility gate.
    #[must_use]
    pub const fn auth(&self) -> &AuthContext {
        &self.auth
    }

    /// Narrows what this session may do.
    ///
    /// Separate from the constructor because a profile can be narrowed after
    /// the fact in a way it cannot be widened. The `2026-07-28` stateless
    /// lifecycle is the reason: every request carries its own version and
    /// capabilities in `_meta` and is served against its own profile, so the
    /// connection settles one profile and each request may hold less.
    ///
    /// Narrowing only, deliberately. Widening would let a caller grant a
    /// session a feature the negotiation never produced, and that is the
    /// "rules-tightening in code" hazard in its most dangerous form: the
    /// refusal it defeats is a refusal the peer was never entitled to.
    #[must_use]
    pub const fn with_profile(mut self, profile: FeatureProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Records who the caller is.
    #[must_use]
    pub fn with_auth(mut self, auth: AuthContext) -> Self {
        self.auth = auth;
        self
    }

    /// The revision this connection settled on.
    #[must_use]
    pub const fn negotiated(&self) -> ProtocolVersion {
        self.negotiated
    }

    /// The profile that revision and the peer's capabilities produced.
    #[must_use]
    pub const fn profile(&self) -> FeatureProfile {
        self.profile
    }

    /// Which session this is.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// Whether `feature` may be used on this connection.
    #[must_use]
    pub fn allows(&self, feature: Feature) -> bool {
        self.profile.allows(feature)
    }

    /// The port for a server-initiated round trip.
    #[must_use]
    pub const fn exchange(&self) -> &Arc<dyn DynExchange> {
        &self.exchange
    }

    /// The port for a one-way notification.
    #[must_use]
    pub const fn sink(&self) -> &Arc<dyn DynFrameSink> {
        &self.sink
    }

    /// The gate, in one place, called by every gated method.
    fn require(&self, feature: Feature) -> Result<(), GatedError> {
        if self.profile.allows(feature) {
            return Ok(());
        }
        Err(GatedError::FeatureUnavailable {
            feature,
            negotiated: self.negotiated,
        })
    }
}

/// Everything a handler is allowed to know about the call it is serving.
#[derive(Clone)]
pub struct RequestContext {
    peer: PeerSession,
    id: RequestId,
    cancel: CancellationToken,
    progress: Option<ProgressToken>,
}

impl core::fmt::Debug for RequestContext {
    /// Prints what a post-mortem needs and nothing else.
    ///
    /// Handwritten because the derived form would print the request id in
    /// full, and a request id in a log line is exactly what the redaction rule
    /// exists to prevent. The id goes through [`RequestId::redacted`].
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RequestContext")
            .field("request", &self.id.redacted())
            .field("negotiated", &self.peer.negotiated)
            .field("has_progress_token", &self.progress.is_some())
            .field("cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl RequestContext {
    /// Binds one in-flight request to the connection serving it.
    ///
    /// Clones the session, which is two `Arc` bumps and two `Copy` fields. That
    /// is why a server with a thousand in-flight requests pays a thousand
    /// pointer bumps rather than a thousand deep copies of the profile.
    #[must_use]
    pub fn for_request(
        peer: &PeerSession,
        id: RequestId,
        cancel: CancellationToken,
        progress: Option<ProgressToken>,
    ) -> Self {
        Self {
            peer: peer.clone(),
            id,
            cancel,
            progress,
        }
    }

    /// The connection serving this request.
    #[must_use]
    pub const fn peer(&self) -> &PeerSession {
        &self.peer
    }

    /// The request id, for correlating anything the handler emits itself.
    #[must_use]
    pub const fn request_id(&self) -> &RequestId {
        &self.id
    }

    /// The caller's cancellation token, so a long handler can observe it.
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancel
    }

    /// The progress token the client opened, if it opened one.
    #[must_use]
    pub const fn progress_token(&self) -> Option<&ProgressToken> {
        self.progress.as_ref()
    }

    /// Asks the peer for extra input from the user.
    ///
    /// Gated on [`Feature::Elicitation`]. The gate runs first, so a session
    /// that never negotiated it writes nothing at all — see
    /// [`GatedError::wrote_nothing`].
    ///
    /// # Errors
    ///
    /// [`GatedError::FeatureUnavailable`] when elicitation was not negotiated,
    /// having written nothing. [`GatedError::MalformedAnswer`] when the
    /// exchange did not complete, which is the one case here where the request
    /// did reach the wire first.
    pub async fn elicit(&self, params: &JsonObject) -> Result<JsonObject, GatedError> {
        self.peer.require(Feature::Elicitation)?;
        self.round_trip("elicitation/create", params).await
    }

    /// Asks the peer to run a model on the server's behalf.
    ///
    /// Gated on [`Feature::Sampling`], on the same terms as [`elicit`].
    ///
    /// # Errors
    ///
    /// As [`elicit`](Self::elicit).
    pub async fn sample(&self, params: &JsonObject) -> Result<JsonObject, GatedError> {
        self.peer.require(Feature::Sampling)?;
        self.round_trip("sampling/createMessage", params).await
    }

    /// Opens a long-running task for this request.
    ///
    /// Gated on [`Feature::Tasks`], which enters the core protocol in
    /// `2025-11-25` and moves to an extension in `2026-07-28` — so the same
    /// call is available, unavailable, and available-elsewhere across three
    /// revisions, and the gate is what makes that legible.
    ///
    /// # Errors
    ///
    /// As [`elicit`](Self::elicit).
    pub async fn create_task(&self, params: &JsonObject) -> Result<JsonObject, GatedError> {
        self.peer.require(Feature::Tasks)?;
        self.round_trip("tasks/create", params).await
    }

    /// Reports progress on a request the client opened a channel for.
    ///
    /// Not feature-gated, and deliberately: progress is core to every revision
    /// from `2024-11-05` onward, so a `Feature` for it would be a fiction.
    /// What can be absent is the _token_ — the client chose whether to open
    /// the channel, and a report without one has nowhere to go.
    ///
    /// # Errors
    ///
    /// [`GatedError::NoProgressToken`] when the client did not open a progress
    /// channel for this request, having written nothing.
    pub async fn report_progress(&self, params: &JsonObject) -> Result<(), GatedError> {
        let Some(token) = self.progress.as_ref() else {
            return Err(GatedError::NoProgressToken);
        };
        let frame = Self::progress_frame(token, params);
        self.peer
            .sink
            .send(&frame)
            .await
            .map_err(|_| GatedError::MalformedAnswer {
                reason: "the progress notification did not reach the peer".to_string(),
            })
    }

    /// Sends the request and reads the answer, mapping a bad answer to the one
    /// error that is not a gate.
    async fn round_trip(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Result<JsonObject, GatedError> {
        self.peer
            .exchange
            .exchange_boxed(method, params)
            .await
            .map_err(|failure| GatedError::MalformedAnswer {
                reason: failure.to_string(),
            })
    }

    /// The JSON-RPC notification a progress report travels in.
    ///
    /// Built with `serde_json` rather than by concatenating into a `String`.
    /// Hand-building a document is how a token containing a quote produces
    /// something that is not JSON — the same defect T09 hit by rendering an id
    /// with `Debug`.
    ///
    /// An associated function rather than a method because it reads nothing
    /// from the context: the token and the caller's params are the whole
    /// message, and a `&self` here would suggest a request id or a profile
    /// leaks into the frame when neither does.
    fn progress_frame(token: &ProgressToken, params: &JsonObject) -> String {
        let mut members = serde_json::Map::new();
        members.insert(
            "progressToken".to_string(),
            serde_json::Value::String(token.to_string()),
        );
        // The caller's params are already validated JSON, so they are spliced
        // in as a value rather than re-escaped. A caller who supplied a
        // `progressToken` of their own loses to the real one: the token is not
        // a hint, it is the routing key, and two of them leave the peer unable
        // to tell which stream a report belongs to.
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(params.as_str()) {
            for (key, value) in map {
                if key != "progressToken" {
                    members.insert(key, value);
                }
            }
        }
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": serde_json::Value::Object(members),
        });
        // A `serde_json::Value` that cannot be written is a bug rather than a
        // runtime condition, and returning a frame that names the failure beats
        // panicking inside a handler the caller cannot catch.
        serde_json::to_string(&frame).unwrap_or_else(|reason| {
            format!(r#"{{"error":"the progress frame could not be serialised: {reason}"}}"#)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{CallbackExchange, ExchangeError, GatedError, PeerSession, RequestContext};
    use crate::application::session::{FrameSink, SinkError};
    use crate::domain::feature::{Feature, FeatureProfile};
    use crate::domain::ir::{JsonObject, JsonObjectError};
    use crate::domain::session::{CancellationToken, ProgressToken, SessionId};
    use crate::domain::{ProtocolVersion, RequestId, TransportKind};

    /// What a test here can fail with.
    ///
    /// A `String` is not enough, because a test needs to fail on a
    /// `JsonObjectError` from a fixture and on its own prose, and the only way
    /// to have both is to name them. Without this, the first `return
    /// Err(format!(..))` fixes the function's error type at `String` and every
    /// later `?` stops compiling.
    #[derive(Debug)]
    enum Failure {
        /// The test's own complaint.
        Message(String),
        /// A fixture was not a JSON object.
        Fixture(JsonObjectError),
        /// A frame the test produced was not JSON.
        Serde(serde_json::Error),
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Message(text) => f.write_str(text),
                Self::Fixture(error) => write!(f, "bad fixture: {error}"),
                Self::Serde(error) => write!(f, "not JSON: {error}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<String> for Failure {
        fn from(text: String) -> Self {
            Self::Message(text)
        }
    }

    impl From<&str> for Failure {
        fn from(text: &str) -> Self {
            Self::Message(text.to_string())
        }
    }

    impl From<JsonObjectError> for Failure {
        fn from(error: JsonObjectError) -> Self {
            Self::Fixture(error)
        }
    }

    impl From<serde_json::Error> for Failure {
        fn from(error: serde_json::Error) -> Self {
            Self::Serde(error)
        }
    }

    impl From<GatedError> for Failure {
        fn from(error: GatedError) -> Self {
            Self::Message(error.to_string())
        }
    }

    type TestError = Failure;

    /// Reads a nested member out of a parsed frame without panicking.
    ///
    /// `serde_json::Value` indexes, and its `Index` impl panics on a missing
    /// key — so `parsed["params"]["progressToken"]` turns a frame that came
    /// out wrong into a test that aborts rather than one that reports what was
    /// wrong with it. Which is the opposite of what a test is for.
    fn at<'a>(
        value: &'a serde_json::Value,
        path: &[&str],
    ) -> Result<&'a serde_json::Value, Failure> {
        let mut cursor = value;
        for (depth, key) in path.iter().enumerate() {
            let object = cursor.as_object().ok_or_else(|| {
                Failure::from(format!(
                    "{path:?} is not an object at depth {depth}, found a {}",
                    match cursor {
                        serde_json::Value::Null => "null",
                        serde_json::Value::Bool(_) => "bool",
                        serde_json::Value::Number(_) => "number",
                        serde_json::Value::String(_) => "string",
                        serde_json::Value::Array(_) => "array",
                        serde_json::Value::Object(_) => "object",
                    }
                ))
            })?;
            cursor = object
                .get(*key)
                .ok_or_else(|| Failure::from(format!("{path:?} has no member {key:?}")))?;
        }
        Ok(cursor)
    }

    /// Counts what reached the wire.
    ///
    /// The whole point of the gate is a claim about what did _not_ happen, and
    /// a mock that only records successes cannot show it. A `Mutex<Vec<_>>`
    /// rather than an `AtomicUsize` because the assertion is on the frames
    /// themselves in the tests that check them.
    #[derive(Default)]
    struct RecordingSink {
        frames: Mutex<Vec<String>>,
    }

    impl RecordingSink {
        fn written(&self) -> Vec<String> {
            // A poisoned lock means another test thread panicked while holding
            // it. Taking the inner value anyway is right here: the vector is
            // append-only and the assertion is on its contents, so a poison
            // from an unrelated failure should not mask this one.
            let frames = self
                .frames
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            frames.clone()
        }
    }

    impl FrameSink for RecordingSink {
        /// Not `async fn`, despite the trait being an RPITIT.
        ///
        /// Recording a frame is a mutex push and nothing else; wrapping it in
        /// an `async` block would build a state machine that is ready
        /// immediately, which is a cost with no reader. Returning
        /// `std::future::ready` says the same thing in one allocation fewer,
        /// and the trait's `impl Future` return is satisfied either way.
        fn send(&self, frame: &str) -> impl Future<Output = Result<(), SinkError>> + Send {
            // Scoped so the guard drops at the end of the block rather than at
            // the end of the function: a MutexGuard held across an await is a
            // bug in an async runtime, and holding it one line longer than
            // needed is the habit that produces one.
            {
                let mut frames = self
                    .frames
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                frames.push(frame.to_string());
            }
            std::future::ready(Ok(()))
        }
    }

    /// Counts exchanges, so a test can prove the gate ran before the round
    /// trip rather than after it.
    #[derive(Default)]
    struct CountingExchange {
        calls: Mutex<Vec<String>>,
        answer: Option<JsonObject>,
    }

    impl CountingExchange {
        fn answering(answer: &str) -> Result<Self, TestError> {
            Ok(Self {
                calls: Mutex::new(Vec::new()),
                answer: Some(JsonObject::new(answer)?),
            })
        }
    }

    impl CallbackExchange for CountingExchange {
        fn exchange(
            &self,
            method: &str,
            _params: &JsonObject,
        ) -> impl Future<Output = Result<JsonObject, ExchangeError>> + Send {
            {
                let mut calls = self
                    .calls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                calls.push(method.to_string());
            }
            let outcome = self
                .answer
                .clone()
                .ok_or_else(|| ExchangeError::new("no answer configured"));
            std::future::ready(outcome)
        }
    }

    /// A peer at `version` carrying `features`.
    fn peer(
        version: ProtocolVersion,
        features: FeatureProfile,
        exchange: &Arc<CountingExchange>,
        sink: &Arc<RecordingSink>,
    ) -> PeerSession {
        PeerSession::new(
            version,
            features,
            SessionId::new(7),
            TransportKind::Stdio,
            exchange.clone(),
            sink.clone(),
        )
    }

    /// The scenario the task file names: elicitation on a session that never
    /// negotiated it.
    ///
    /// Three claims, and the second is the one that matters. The error has to
    /// name the feature, or a handler cannot fall back. The refusal has to
    /// write nothing, or the peer has already seen a request the protocol does
    /// not permit at this revision. And the exchange must not have been
    /// called, because "the gate refused" and "the gate ran after the round
    /// trip and threw the answer away" produce the same error and opposite
    /// behaviour.
    #[tokio::test]
    async fn elicitation_on_an_old_session_is_refused_and_writes_nothing() -> Result<(), TestError>
    {
        let exchange = Arc::new(CountingExchange::answering(r#"{"action":"accept"}"#)?);
        let sink = Arc::new(RecordingSink::default());

        // 2025-03-26 has no elicitation. The profile is built from the real
        // resolver rather than hand-listed, so a change to the matrix moves
        // this test instead of quietly invalidating its premise.
        let profile = crate::domain::feature::resolve(crate::domain::feature::Negotiation::bare(
            ProtocolVersion::V2025_03_26,
            crate::domain::TransportKind::Stdio,
        ));
        assert!(
            !profile.allows(Feature::Elicitation),
            "the premise of this test is that 2025-03-26 has no elicitation"
        );

        let peer = peer(ProtocolVersion::V2025_03_26, profile, &exchange, &sink);
        let ctx = RequestContext::for_request(
            &peer,
            RequestId::number(1),
            CancellationToken::new(),
            None,
        );
        let params = JsonObject::new(r#"{"message":"name?"}"#)?;

        let refusal = match ctx.elicit(&params).await {
            Ok(answer) => {
                return Err(Failure::from(format!(
                    "elicit unexpectedly returned {answer}"
                )));
            }
            Err(refusal) => refusal,
        };

        assert_eq!(
            refusal,
            GatedError::FeatureUnavailable {
                feature: Feature::Elicitation,
                negotiated: ProtocolVersion::V2025_03_26,
            }
        );
        assert!(refusal.wrote_nothing());
        assert_eq!(refusal.feature(), Some(Feature::Elicitation));
        assert!(
            sink.written().is_empty(),
            "the gate must run before the sink is touched"
        );
        assert!(
            exchange
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty(),
            "the gate must run before the exchange is reached"
        );
        Ok(())
    }

    /// The same gate, permitted, has to actually reach the peer — otherwise a
    /// test suite full of refusals would pass against an implementation that
    /// never sends anything.
    #[tokio::test]
    async fn a_negotiated_gate_reaches_the_peer() -> Result<(), TestError> {
        let exchange = Arc::new(CountingExchange::answering(r#"{"action":"accept"}"#)?);
        let sink = Arc::new(RecordingSink::default());
        let peer = peer(
            ProtocolVersion::V2025_06_18,
            FeatureProfile::empty().with(Feature::Elicitation),
            &exchange,
            &sink,
        );
        let ctx = RequestContext::for_request(
            &peer,
            RequestId::number(1),
            CancellationToken::new(),
            None,
        );
        let params = JsonObject::new(r#"{"message":"name?"}"#)?;

        let answer = match ctx.elicit(&params).await {
            Ok(answer) => answer,
            Err(error) => return Err(Failure::from(error)),
        };

        assert_eq!(answer.as_str(), r#"{"action":"accept"}"#);
        assert_eq!(
            exchange
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            vec!["elicitation/create".to_string()]
        );
        Ok(())
    }

    /// Every gated call is refused the same way, and names itself.
    ///
    /// Table-driven over the three, because a gate added later and wired to
    /// the wrong feature is exactly the bug a single hand-written test for
    /// one of them would miss.
    #[tokio::test]
    async fn every_gated_call_refuses_when_its_feature_is_absent() -> Result<(), TestError> {
        let cases: [(Feature, &str); 3] = [
            (Feature::Elicitation, "elicitation/create"),
            (Feature::Sampling, "sampling/createMessage"),
            (Feature::Tasks, "tasks/create"),
        ];

        for (feature, method) in cases {
            let exchange = Arc::new(CountingExchange::answering("{}")?);
            let sink = Arc::new(RecordingSink::default());
            let peer = peer(
                ProtocolVersion::V2024_11_05,
                FeatureProfile::empty(),
                &exchange,
                &sink,
            );
            let ctx = RequestContext::for_request(
                &peer,
                RequestId::number(1),
                CancellationToken::new(),
                None,
            );
            let params = JsonObject::new("{}")?;

            let outcome = match feature {
                Feature::Elicitation => ctx.elicit(&params).await,
                Feature::Sampling => ctx.sample(&params).await,
                Feature::Tasks => ctx.create_task(&params).await,
                _ => {
                    return Err(Failure::from(
                        "the table named a feature with no gated call",
                    ));
                }
            };

            let refusal = match outcome {
                Ok(answer) => {
                    return Err(Failure::from(format!(
                        "{method} unexpectedly returned {answer}"
                    )));
                }
                Err(refusal) => refusal,
            };

            assert_eq!(
                refusal.feature(),
                Some(feature),
                "{method} refused for the wrong feature"
            );
            assert!(sink.written().is_empty(), "{method} wrote before refusing");
        }
        Ok(())
    }

    /// Progress is refused for a different reason than the feature gates, and
    /// the reason matters: the fix is the client's token, not the profile.
    #[tokio::test]
    async fn progress_without_a_token_refuses_without_touching_the_sink() -> Result<(), TestError> {
        let exchange = Arc::new(CountingExchange::answering("{}")?);
        let sink = Arc::new(RecordingSink::default());
        let peer = peer(
            ProtocolVersion::V2025_06_18,
            FeatureProfile::all(),
            &exchange,
            &sink,
        );
        let ctx = RequestContext::for_request(
            &peer,
            RequestId::number(1),
            CancellationToken::new(),
            None,
        );
        let params = JsonObject::new(r#"{"progress":0.5}"#)?;

        let refusal = match ctx.report_progress(&params).await {
            Ok(()) => {
                return Err(Failure::from("report_progress unexpectedly succeeded"));
            }
            Err(refusal) => refusal,
        };

        assert_eq!(refusal, GatedError::NoProgressToken);
        assert!(refusal.wrote_nothing());
        assert!(refusal.feature().is_none());
        assert!(sink.written().is_empty());
        Ok(())
    }

    /// A progress report that does have a token reaches the sink, with the
    /// token spliced in rather than the caller's guess at it.
    #[tokio::test]
    async fn a_progress_report_carries_the_real_token() -> Result<(), TestError> {
        let exchange = Arc::new(CountingExchange::answering("{}")?);
        let sink = Arc::new(RecordingSink::default());
        let peer = peer(
            ProtocolVersion::V2025_06_18,
            FeatureProfile::all(),
            &exchange,
            &sink,
        );
        let ctx = RequestContext::for_request(
            &peer,
            RequestId::number(1),
            CancellationToken::new(),
            Some(ProgressToken::text("real-token")),
        );
        // A caller that supplies its own progressToken must not be able to
        // redirect the report onto a stream the peer did not open.
        let params = JsonObject::new(r#"{"progressToken":"forged","progress":0.5}"#)?;

        ctx.report_progress(&params).await?;

        let frames = sink.written();
        let frame = frames.first().ok_or("the sink received nothing")?;
        let parsed: serde_json::Value =
            serde_json::from_str(frame).map_err(|e| format!("the frame is not JSON: {e}"))?;

        assert_eq!(at(&parsed, &["method"])?, "notifications/progress");
        assert_eq!(at(&parsed, &["params", "progressToken"])?, "real-token");
        assert_eq!(at(&parsed, &["params", "progress"])?, 0.5);
        assert!(
            at(&parsed, &["params", "forged"]).is_err(),
            "the caller's extra members should not appear"
        );
        Ok(())
    }

    /// A token containing a quote must not produce a frame that is not JSON.
    ///
    /// This is the T09 `Debug`-escaping defect, reached from the other
    /// direction: the frame is assembled by string concatenation in the
    /// obvious implementation, and a progress token is caller-supplied.
    #[tokio::test]
    async fn a_quote_in_the_token_still_produces_valid_json() -> Result<(), TestError> {
        let exchange = Arc::new(CountingExchange::answering("{}")?);
        let sink = Arc::new(RecordingSink::default());
        let peer = peer(
            ProtocolVersion::V2025_06_18,
            FeatureProfile::all(),
            &exchange,
            &sink,
        );
        let ctx = RequestContext::for_request(
            &peer,
            RequestId::number(1),
            CancellationToken::new(),
            Some(ProgressToken::text(r#"a"b\c"#)),
        );

        ctx.report_progress(&JsonObject::new("{}")?).await?;

        let frames = sink.written();
        let frame = frames.first().ok_or("the sink received nothing")?;
        let parsed: serde_json::Value =
            serde_json::from_str(frame).map_err(|e| format!("the frame is not JSON: {e}"))?;
        assert_eq!(at(&parsed, &["params", "progressToken"])?, r#"a"b\c"#);
        Ok(())
    }

    /// The debug rendering of a context must not carry a usable request id.
    ///
    /// The redaction rule names session ids and tokens; a request id is the
    /// same class of value, and the derived `Debug` would print it whole
    /// because `RequestId` has a `Debug` that a log line will call.
    #[test]
    fn a_context_does_not_print_its_request_id_in_full() -> Result<(), TestError> {
        let exchange = Arc::new(CountingExchange::answering("{}")?);
        let sink = Arc::new(RecordingSink::default());
        let peer = peer(
            ProtocolVersion::V2025_06_18,
            FeatureProfile::all(),
            &exchange,
            &sink,
        );
        let ctx = RequestContext::for_request(
            &peer,
            RequestId::text("secret-request-id"),
            CancellationToken::new(),
            None,
        );

        let rendered = format!("{ctx:?}");
        assert!(
            !rendered.contains("secret-request-id"),
            "the debug form leaked the request id: {rendered}"
        );
        assert!(rendered.contains("RequestContext"));
        Ok(())
    }
}
