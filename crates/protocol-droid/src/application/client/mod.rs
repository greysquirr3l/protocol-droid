//! [`Client`] — the calling side of a negotiated session.
//!
//! # What it is
//!
//! A client holds the revisions it will speak, in preference order, and the
//! features it wants. It does not open a connection: a transport does that, and
//! until T13 supplies one, a test can hand the client a closure directly.
//!
//! # The gate, and where it lives
//!
//! Every typed method consults the resolved [`FeatureProfile`] before it does
//! anything, exactly as the server's
//! [`RequestContext`](crate::application::server::RequestContext) does. The
//! asymmetry is deliberate: the server's gate protects the _peer_ from a
//! feature the server did not negotiate, and the client's gate protects the
//! _caller_ from a round trip that cannot work.
//!
//! Both produce the same [`ClientError`], because from the caller's side
//! "this session cannot do that" is one fact with one fix — configure a
//! version that can, or drop the call.

use std::sync::Arc;

use crate::domain::feature::{Feature, FeatureProfile, Negotiation, resolve};
use crate::domain::ir::JsonObject;
use crate::domain::{CapabilitySet, ProtocolVersion, TransportKind};

/// Sends one request to the peer and returns its answer.
///
/// The port a client needs, and the only one. A transport implements it; the
/// client does not know about sockets, headers, or framing.
///
/// The boxed form because a client's exchange is chosen at run time from a
/// configuration and a test substitutes a different one — there is no
/// monomorphisation to win here, and holding a `dyn` is the honest shape.
pub trait ClientTransport: Send + Sync {
    /// Sends `method` with `params` and yields the peer's answer.
    ///
    /// # Errors
    ///
    /// [`TransportError`] when the request could not be delivered or answered.
    /// A client that waited forever would be a caller that cannot be
    /// interrupted, so a non-answer is an error rather than a hang.
    fn send(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>>;
}

/// Why a request did not complete.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The transport could not deliver the request, or the peer did not
    /// answer it.
    ///
    /// A `String` for the same reason the server's
    /// [`ExchangeError`](crate::application::server::ExchangeError) is: the
    /// transport names its own failures and the application layer has no
    /// business enumerating them.
    #[error("{0}")]
    Unavailable(String),
}

/// Why a client call did not happen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// The negotiated session does not have the feature the call needs.
    ///
    /// The revision is carried for the same reason the server's is: "sampling
    /// is unavailable" is not something a caller can act on without knowing
    /// what _was_ negotiated.
    #[error("{feature} was not negotiated (session is {negotiated})")]
    FeatureUnavailable {
        /// The feature the call needed.
        feature: Feature,
        /// The revision this session settled on.
        negotiated: ProtocolVersion,
    },

    /// The request could not be delivered or answered.
    #[error("the request did not complete: {0}")]
    Transport(#[from] TransportError),
}

impl ClientError {
    /// The feature this refusal is about, when there is one.
    #[must_use]
    pub const fn feature(&self) -> Option<Feature> {
        match self {
            Self::FeatureUnavailable { feature, .. } => Some(*feature),
            Self::Transport(_) => None,
        }
    }
}

/// What a client offers a server, before anything is negotiated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// The revisions this client speaks, most preferred first.
    ///
    /// An ordered list rather than one version because a client that can speak
    /// three revisions should say so and take the best the server agrees to,
    /// rather than pick one and fail when the server prefers another. A
    /// single-element list is the "I only speak this" case and needs no
    /// separate type.
    versions: Vec<ProtocolVersion>,
    capabilities: CapabilitySet,
    transport: TransportKind,
}

impl ClientConfig {
    /// A client that speaks `version`, with nothing advertised.
    #[must_use]
    pub fn new(version: ProtocolVersion, transport: TransportKind) -> Self {
        Self {
            versions: vec![version],
            capabilities: CapabilitySet::empty(),
            transport,
        }
    }

    /// A client that speaks several, most preferred first.
    ///
    /// # Errors
    ///
    /// Refuses an empty list, because a client that speaks no revision cannot
    /// negotiate and would otherwise fail much later with a message about
    /// something else entirely.
    pub fn speaking(
        versions: impl IntoIterator<Item = ProtocolVersion>,
        transport: TransportKind,
    ) -> Result<Self, ClientError> {
        let versions: Vec<ProtocolVersion> = versions.into_iter().collect();
        if versions.is_empty() {
            return Err(ClientError::Transport(TransportError::Unavailable(
                "a client must offer at least one revision".to_string(),
            )));
        }
        Ok(Self {
            versions,
            capabilities: CapabilitySet::empty(),
            transport,
        })
    }

    /// Declares the capabilities the client supports.
    #[must_use]
    pub const fn with_capabilities(mut self, capabilities: CapabilitySet) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// The revisions offered, most preferred first.
    #[must_use]
    pub fn versions(&self) -> &[ProtocolVersion] {
        &self.versions
    }

    /// The transport the client will run over.
    #[must_use]
    pub const fn transport(&self) -> TransportKind {
        self.transport
    }

    /// The negotiation this offer produces against a server's answer.
    ///
    /// The one place the two halves are put together. A caller that already
    /// knows the version and the server's capabilities — which is what a
    /// transport knows once the handshake is done — passes them here rather
    /// than leaving this type to invent a half-negotiation.
    #[must_use]
    pub const fn negotiated_with(
        &self,
        version: ProtocolVersion,
        server_caps: CapabilitySet,
    ) -> Negotiation {
        Negotiation {
            version,
            client_caps: self.capabilities,
            server_caps,
            transport: self.transport,
        }
    }
}

/// A client bound to a negotiated session.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use protocol_droid::application::client::{Client, ClientConfig, TransportError};
/// use protocol_droid::domain::ir::JsonObject;
/// use protocol_droid::domain::{ProtocolVersion, TransportKind};
///
/// /// A transport that always refuses, which is enough to show the gate.
/// struct Dead;
/// # impl protocol_droid::application::client::ClientTransport for Dead {
/// #     fn send(&self, _: &str, _: &JsonObject)
/// #         -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send>>
/// #     { Box::pin(async { Err(TransportError::Unavailable("no route".to_string())) }) }
/// # }
///
/// let version = ProtocolVersion::V2024_11_05;
/// let config = ClientConfig::new(version, TransportKind::Stdio);
/// let client = Client::connect(
///     config.negotiated_with(version, protocol_droid::domain::CapabilitySet::all_for(version)),
///     Arc::new(Dead),
/// );
///
/// // Tools exist in every revision, and both sides declared everything that
/// // revision defines, so the gate is open and the dead transport is what
/// // fails the call.
/// assert!(client.supports(protocol_droid::domain::Feature::Tools));
///
/// let outcome = tokio::runtime::Builder::new_current_thread()
///     .build()
///     .map_err(|e| e.to_string())?
///     .block_on(client.list_tools());
/// assert_eq!(
///     outcome.map_or_else(|e| e.to_string(), |_| "succeeded".to_string()),
///     "the request did not complete: no route",
/// );
/// # Ok::<(), String>(())
/// ```
#[derive(Clone)]
pub struct Client {
    negotiated: ProtocolVersion,
    profile: FeatureProfile,
    transport: Arc<dyn ClientTransport>,
}

impl core::fmt::Debug for Client {
    /// Prints the negotiation, not the transport.
    ///
    /// For the same reason `PeerSession` does: a transport is a socket and a
    /// vtable pointer, and neither survives being printed at a call site.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Client")
            .field("negotiated", &self.negotiated)
            .field("feature_count", &self.profile.len())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Binds a transport to a session that has already negotiated.
    ///
    /// Takes the whole [`Negotiation`] rather than just the version. The
    /// profile is a function of four inputs, and a client knows only two of
    /// them — its own capabilities and the transport. Re-deriving it here
    /// would mean guessing the server's half, and a guess that leaves the
    /// server's capabilities empty turns a feature nobody declared into a
    /// feature nobody may use. The transport did the handshake and has the
    /// real answer, so it passes the real answer.
    #[must_use]
    pub fn connect(negotiation: Negotiation, transport: Arc<dyn ClientTransport>) -> Self {
        Self {
            negotiated: negotiation.version,
            profile: resolve(negotiation),
            transport,
        }
    }

    /// The revision this client and its server agreed on.
    #[must_use]
    pub const fn negotiated(&self) -> ProtocolVersion {
        self.negotiated
    }

    /// The profile that revision produced.
    #[must_use]
    pub const fn profile(&self) -> FeatureProfile {
        self.profile
    }

    /// Whether `feature` may be used on this session.
    #[must_use]
    pub fn supports(&self, feature: Feature) -> bool {
        self.profile.allows(feature)
    }

    /// Lists the server's tools.
    ///
    /// # Errors
    ///
    /// [`ClientError::FeatureUnavailable`] when the session has no
    /// [`Feature::Tools`], having written nothing. A 2024-11-05 session always
    /// has one, but a session whose peer advertises nothing still might not,
    /// and a caller learns which from this rather than from an opaque
    /// protocol error.
    pub async fn list_tools(&self) -> Result<JsonObject, ClientError> {
        self.require(Feature::Tools)?;
        self.request("tools/list", "{}").await
    }

    /// Calls `name` with `arguments`.
    ///
    /// # Errors
    ///
    /// As [`list_tools`](Self::list_tools).
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &JsonObject,
    ) -> Result<JsonObject, ClientError> {
        self.require(Feature::Tools)?;
        // `arguments` is a validated `JsonObject`, so re-parsing it cannot
        // fail. It has to be re-parsed at all: `JsonObject` is opaque text by
        // design, and nesting one object inside another is the one operation
        // that needs its structure back. The fallback is the empty object
        // rather than an error, because a value that arrived here as a
        // `JsonObject` has already been checked once and a second failure
        // would report the same malformation from two places.
        let nested = serde_json::from_str::<serde_json::Value>(arguments.as_str())
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
        let params = JsonObject::from_value(serde_json::json!({
            "name": name,
            "arguments": nested,
        }))
        .unwrap_or_default();
        self.transport
            .send("tools/call", &params)
            .await
            .map_err(ClientError::from)
    }

    /// Lists the server's resources.
    ///
    /// # Errors
    ///
    /// As [`list_tools`](Self::list_tools), with
    /// [`Feature::Resources`](Feature) in place of
    /// [`Feature::Tools`](Feature).
    pub async fn list_resources(&self) -> Result<JsonObject, ClientError> {
        self.require(Feature::Resources)?;
        self.request("resources/list", "{}").await
    }

    /// Lists the server's prompts.
    ///
    /// # Errors
    ///
    /// As [`list_tools`](Self::list_tools), with
    /// [`Feature::Prompts`](Feature) in place of
    /// [`Feature::Tools`](Feature).
    pub async fn list_prompts(&self) -> Result<JsonObject, ClientError> {
        self.require(Feature::Prompts)?;
        self.request("prompts/list", "{}").await
    }

    /// The gate, in one place, called by every typed method.
    fn require(&self, feature: Feature) -> Result<(), ClientError> {
        if self.profile.allows(feature) {
            return Ok(());
        }
        Err(ClientError::FeatureUnavailable {
            feature,
            negotiated: self.negotiated,
        })
    }

    /// Sends a request whose gate has already been checked.
    async fn request(&self, method: &str, params: &str) -> Result<JsonObject, ClientError> {
        let owned = match JsonObject::new(params) {
            Ok(object) => object,
            Err(error) => {
                return Err(ClientError::Transport(TransportError::Unavailable(format!(
                    "{method} was given parameters that are not a JSON object: {error}"
                ))));
            }
        };
        self.transport.send(method, &owned).await.map_err(ClientError::from)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{Client, ClientConfig, ClientError, ClientTransport, TransportError};
    use crate::domain::feature::Feature;
    use crate::domain::ir::{JsonObject, JsonObjectError};
    use crate::domain::{CapabilitySet, ProtocolVersion, TransportKind};

    /// What a test here can fail with.
    #[derive(Debug)]
    enum Failure {
        /// The test's own complaint.
        Message(String),
        /// A fixture was not a JSON object.
        Fixture(JsonObjectError),
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

    type TestError = Failure;

    /// A transport that records what it was asked and answers with a fixed
    /// object.
    struct Recorder {
        calls: Mutex<Vec<String>>,
        answer: Option<JsonObject>,
    }

    impl Recorder {
        fn answering(answer: &str) -> Result<Self, TestError> {
            Ok(Self {
                calls: Mutex::new(Vec::new()),
                answer: Some(JsonObject::new(answer)?),
            })
        }

        fn dead() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                answer: None,
            }
        }

        fn called(&self) -> Vec<String> {
            let calls = self
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            calls.clone()
        }
    }

    impl ClientTransport for Recorder {
        fn send(
            &self,
            method: &str,
            _params: &JsonObject,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>>
        {
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
                .ok_or_else(|| TransportError::Unavailable("nothing listening".to_string()));
            Box::pin(std::future::ready(outcome))
        }
    }

    /// A client at `version`, with both sides declaring everything that
    /// revision defines.
    ///
    /// `all_for` rather than a hand-listed set, so a change to the capability
    /// matrix moves this instead of quietly invalidating the premise.
    fn client_at(version: ProtocolVersion, transport: Arc<Recorder>) -> Client {
        let caps = CapabilitySet::all_for(version);
        let config = ClientConfig::new(version, TransportKind::Stdio);
        Client::connect(config.negotiated_with(version, caps), transport)
    }

    /// A gated call that cannot run writes nothing and names the feature.
    ///
    /// The transport is `dead`, so a call that got through would fail with
    /// `Transport` — which is what makes this a test of the gate and not of
    /// the transport. The error variant is the whole assertion.
    #[tokio::test]
    async fn a_gated_call_on_a_session_without_it_never_reaches_the_transport(
    ) -> Result<(), TestError> {
        // 2024-11-05 has elicitation only in... it does not have it at all.
        // Resolving with empty capabilities on both sides is the honest way to
        // say "this session negotiated nothing optional".
        let transport = Arc::new(Recorder::dead());
        let version = ProtocolVersion::V2024_11_05;
        let config = ClientConfig::new(version, TransportKind::Stdio);
        let client = Client::connect(
            config.negotiated_with(version, CapabilitySet::empty()),
            transport.clone(),
        );

        let refusal = match client.list_prompts().await {
            Ok(answer) => {
                return Err(Failure::from(format!("expected a refusal, got {answer}")));
            }
            Err(error) => error,
        };

        assert_eq!(
            refusal,
            ClientError::FeatureUnavailable {
                feature: Feature::Prompts,
                negotiated: ProtocolVersion::V2024_11_05,
            }
        );
        assert_eq!(refusal.feature(), Some(Feature::Prompts));
        assert!(transport.called().is_empty());
        Ok(())
    }

    /// An ungated call reaches the transport with the right method name.
    ///
    /// The other half of the gate's contract: a test suite of refusals would
    /// pass just as happily against a client that never sends anything.
    #[tokio::test]
    async fn a_permitted_call_reaches_the_transport() -> Result<(), TestError> {
        let version = ProtocolVersion::V2025_06_18;
        let transport = Arc::new(Recorder::answering(r#"{"tools":[]}"#)?);
        let client = client_at(version, transport.clone());

        let answer = match client.list_tools().await {
            Ok(answer) => answer,
            Err(error) => return Err(Failure::from(error.to_string())),
        };

        assert_eq!(answer.as_str(), r#"{"tools":[]}"#);
        assert_eq!(transport.called(), vec!["tools/list".to_string()]);
        Ok(())
    }

    /// Each typed method sends its own method name.
    ///
    /// A copy-paste that sent `tools/list` for a resource listing would pass
    /// every other test here and break only against a real server, so the
    /// names are checked together rather than one call at a time.
    #[tokio::test]
    async fn each_typed_method_sends_its_own_method() -> Result<(), TestError> {
        let version = ProtocolVersion::V2025_06_18;
        let transport = Arc::new(Recorder::answering("{}")?);
        let client = client_at(version, transport.clone());

        for outcome in [
            client.list_tools().await.map(|_| ()),
            client.list_resources().await.map(|_| ()),
            client.list_prompts().await.map(|_| ()),
        ] {
            if let Err(error) = outcome {
                return Err(Failure::from(error.to_string()));
            }
        }

        assert_eq!(
            transport.called(),
            vec![
                "tools/list".to_string(),
                "resources/list".to_string(),
                "prompts/list".to_string(),
            ]
        );
        Ok(())
    }

    /// A client that offers no revisions is refused at construction.
    ///
    /// Before the negotiation, not during it: a client that cannot say what it
    /// speaks should not produce a session at all.
    #[test]
    fn a_client_offering_no_revisions_is_refused() -> Result<(), TestError> {
        let empty: Vec<ProtocolVersion> = Vec::new();
        let config = ClientConfig::speaking(empty, TransportKind::Stdio);

        match config {
            Ok(_) => Err(Failure::from(
                "a client offering no revisions was accepted",
            )),
            Err(_) => Ok(()),
        }
    }

    /// The profile a client reports is the one the handshake produced.
    ///
    /// The point of taking the whole `Negotiation` rather than guessing the
    /// server's half: a client that invented an empty server capability set
    /// would report a profile stricter than the real session, and this pins
    /// the difference.
    #[test]
    fn the_client_reports_the_negotiations_profile() {
        let version = ProtocolVersion::V2025_06_18;
        let transport = Arc::new(Recorder::dead());
        let config = ClientConfig::new(version, TransportKind::Stdio);

        let generous = Client::connect(
            config.negotiated_with(version, CapabilitySet::all_for(version)),
            transport.clone(),
        );
        let stingy = Client::connect(
            config.negotiated_with(version, CapabilitySet::empty()),
            transport,
        );

        assert_eq!(generous.negotiated(), version);
        assert!(
            generous.supports(Feature::Resources),
            "both sides declared everything {version} defines"
        );
        assert!(
            !stingy.supports(Feature::Resources),
            "a server that declared nothing offers no resources"
        );
    }
}
