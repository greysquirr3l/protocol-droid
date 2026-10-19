//! Opening a session with an MCP server, from a [`TransportSpec`].
//!
//! The four steps a caller used to write by hand — build the adapter, choose
//! the revisions to offer, run the handshake, bind the result to a [`Client`] —
//! are one step here. None of them is a decision the caller should be making:
//! which adapter speaks a given spec is a fact about the crate, and the set of
//! revisions to offer is a matter of what this build has wire types for.
//!
//! This is the client counterpart of [`serve_http`](crate::application::server::serve::StdioBuilder)
//! and [`serve_stdio`](crate::application::server::serve::StdioBuilder), and it
//! exists for the same reason: a consumer that has to assemble a transport by
//! hand is doing the crate's work, and doing it once per project.
//!
//! [`ClientTransport`]: super::ClientTransport
//! [`Client`]: super::Client

use std::sync::Arc;

use crate::domain::{
    NegotiationError, SpecError, SupportedVersions, SupportedVersionsError, TransportKind,
    TransportSpec,
};

use super::handler::ConnectOptions;
use super::{Client, ClientError, ClientTransport, HandshakeError};
#[cfg(any(feature = "http", feature = "legacy-sse"))]
use super::{ClientConfig, handshake};

/// Why a session could not be opened.
///
/// Collapses five failure sources into one so a caller can handle "could not
/// connect" as one arm, and read *which* of the four stages failed by matching
/// the variant. The inner errors are kept rather than flattened to strings,
/// because a caller retrying on a transient transport fault needs to tell that
/// apart from a server that will never speak a common revision.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The spec names a transport this crate has no client adapter for.
    ///
    /// A refusal rather than a half-open connection: a caller told only that
    /// connecting failed cannot tell "wrong URL" from "this build has no such
    /// adapter", and retries the first forever.
    #[error("this build has no client adapter for `{kind}`: {reason}")]
    Unsupported {
        /// The transport that was asked for.
        kind: TransportKind,
        /// Why it cannot be opened.
        reason: &'static str,
    },

    /// The spec itself was unusable.
    #[error("the transport spec is not usable: {0}")]
    Spec(#[from] SpecError),

    /// The offered revisions were not a usable set.
    #[error("the offered revisions are not usable: {0}")]
    Config(#[from] ClientError),

    /// The default offered revisions could not be built.
    #[error("this build's own revision table is unusable: {0}")]
    Versions(#[from] SupportedVersionsError),

    /// The handshake did not complete.
    #[error("the handshake did not complete: {0}")]
    Handshake(#[from] HandshakeError),

    /// The two sides share no revision.
    #[error("the client and the server share no revision: {0}")]
    NoCommonVersion(#[from] NegotiationError),

    /// A Streamable HTTP endpoint could not be prepared.
    #[cfg(feature = "http")]
    #[error("the Streamable HTTP endpoint could not be prepared: {0}")]
    HttpEndpoint(#[from] crate::adapters::secondary::transport::http::HttpClientError),

    /// A legacy HTTP+SSE endpoint could not be opened.
    #[cfg(feature = "legacy-sse")]
    #[error("the legacy HTTP+SSE endpoint could not be opened: {0}")]
    LegacySse(#[from] crate::adapters::secondary::transport::legacy_sse::LegacySseError),
}

/// Opens `spec`, negotiates, and binds the result.
///
/// Returns the transport alongside the client because several callers need it
/// for something the client does not do — the bridge wraps it as a peer and
/// forwards frames with it, where reading tools over it would be the wrong
/// shape entirely. Handing back the same `Arc` the client holds is what keeps
/// there from being two connections that could disagree about what was
/// negotiated.
///
/// # Errors
///
/// [`ConnectError`] naming the stage that failed.
#[cfg(any(feature = "http", feature = "legacy-sse"))]
pub async fn open(
    spec: &TransportSpec,
    versions: &SupportedVersions,
) -> Result<(Client, Arc<dyn ClientTransport>), ConnectError> {
    open_with(spec, versions, ConnectOptions::new()).await
}

/// [`open`], applying `options`.
///
/// What a client that can answer sampling, elicitation or roots needs: a
/// server only asks for what the client said it can do, and a client that
/// declared nothing is never asked.
///
/// # Errors
///
/// As [`open`].
#[cfg(any(feature = "http", feature = "legacy-sse"))]
pub(super) async fn open_with(
    spec: &TransportSpec,
    versions: &SupportedVersions,
    options: ConnectOptions,
) -> Result<(Client, Arc<dyn ClientTransport>), ConnectError> {
    let capabilities = options.capabilities;
    // One match, so there is exactly one place in the crate that knows which
    // transports have client adapters. A second copy of this table is a second
    // opinion on that question, and it is the copy that has to be remembered
    // when a kind is added.
    // The concrete HTTP client, kept when a handler is wired so its standalone
    // stream can be opened once the handshake has given it a session.
    #[cfg(feature = "http")]
    let listener: Option<Arc<crate::adapters::secondary::transport::http::HttpClient>>;
    let (transport, kind): (Arc<dyn ClientTransport>, TransportKind) = match spec.kind() {
        #[cfg(feature = "http")]
        TransportKind::StreamableHttp => {
            use crate::adapters::secondary::transport::http::{HttpClient, HttpClientConfig};
            let config = HttpClientConfig::new(spec.target())?;
            let mut client = HttpClient::new(config)?;
            if let Some(handler) = options.handler.clone() {
                client = client.answering(handler);
            }
            if let Some(auth) = options.auth.clone() {
                client = client.authorizing(auth);
            }
            let client = Arc::new(client);
            listener = options.handler.is_some().then(|| Arc::clone(&client));
            (client, TransportKind::StreamableHttp)
        }
        #[cfg(feature = "http")]
        TransportKind::StatelessHttp => {
            return open_stateless(spec, versions, options).await;
        }
        #[cfg(feature = "legacy-sse")]
        TransportKind::LegacySse => {
            use crate::adapters::secondary::transport::legacy_sse::{
                LegacySseClient, LegacySseConfig,
            };
            let config = LegacySseConfig::new(spec.target())?;
            let client = LegacySseClient::connect(config).await?;
            listener = None;
            (Arc::new(client), TransportKind::LegacySse)
        }
        kind => {
            return Err(ConnectError::Unsupported {
                kind,
                reason: missing_reason(kind),
            });
        }
    };

    // Newest first, because the first revision is the one offered: a set sorted
    // oldest-first would open every connection by asking for 2024-11-05. Only
    // revisions that have a handshake: a stateful transport that offered
    // `2026-07-28` would send `server/discover` to a server expecting
    // `initialize`. The stateless transport has its own path above.
    let mut offered: Vec<crate::domain::ProtocolVersion> = versions
        .as_slice()
        .iter()
        .copied()
        .filter(|version| crate::domain::handshake_defined_in(*version))
        .collect();
    offered.sort_by(|left, right| right.cmp(left));
    if offered.is_empty() {
        return Err(ConnectError::Unsupported {
            kind,
            reason: "none of the offered revisions has an `initialize` handshake; \
                     use an `http-stateless:` spec for 2026-07-28",
        });
    }
    let config = ClientConfig::speaking(offered, kind)?.with_capabilities(capabilities);
    let session = handshake(transport.as_ref(), &config).await?;
    #[cfg(feature = "http")]
    if let Some(listener) = &listener {
        listener.spawn_listener();
    }

    // The profile is the session's, not re-resolved from the two capability
    // sets. `resolve` is pure and would land in the same place, but reading it
    // off the session is the claim that this is what the server agreed to
    // rather than a second derivation of it that could drift.
    Ok((
        Client::from_session(&session, Arc::clone(&transport)),
        transport,
    ))
}

/// Opens `spec` in a build that compiled no client adapter at all.
///
/// A separate definition rather than one match whose arms were all removed,
/// because a match with nothing left in it does not type — and because "this
/// build has no client adapter for any transport" is a different thing to tell
/// a caller than "that transport has no adapter", and worth saying plainly.
///
/// # Errors
///
/// Always [`ConnectError::Unsupported`].
#[cfg(not(any(feature = "http", feature = "legacy-sse")))]
pub async fn open(
    spec: &TransportSpec,
    versions: &SupportedVersions,
) -> Result<(Client, Arc<dyn ClientTransport>), ConnectError> {
    open_with(spec, versions, ConnectOptions::new()).await
}

/// [`open_with`] in a build with no client adapter: always refuses.
///
/// # Errors
///
/// Always [`ConnectError::Unsupported`].
#[cfg(not(any(feature = "http", feature = "legacy-sse")))]
pub(super) async fn open_with(
    spec: &TransportSpec,
    versions: &SupportedVersions,
    options: ConnectOptions,
) -> Result<(Client, Arc<dyn ClientTransport>), ConnectError> {
    let _ = (versions, options);
    Err(ConnectError::Unsupported {
        kind: spec.kind(),
        reason: missing_reason(spec.kind()),
    })
}

/// Opens a `2026-07-28` session: no `initialize`, one revision per attempt.
///
/// Separate from [`open_with`] because it is a different lifecycle rather than
/// a different transport: it negotiates by trying and being refused, where the
/// stateful path sends a handshake and reads the answer.
#[cfg(feature = "http")]
async fn open_stateless(
    spec: &TransportSpec,
    versions: &SupportedVersions,
    options: ConnectOptions,
) -> Result<(Client, Arc<dyn ClientTransport>), ConnectError> {
    use crate::adapters::secondary::transport::http::{HttpClientConfig, StatelessHttpClient};
    let capabilities = options.capabilities;
    // Only the revisions with no handshake can be spoken statelessly,
    // and the handshake below sends `initialize` for any other — so
    // the offer is narrowed here, newest first.
    let mut stateless: Vec<_> = versions
        .as_slice()
        .iter()
        .copied()
        .filter(|version| !crate::domain::handshake_defined_in(*version))
        .collect();
    stateless.sort_by(|left, right| right.cmp(left));
    if stateless.is_empty() {
        return Err(ConnectError::Unsupported {
            kind: TransportKind::StatelessHttp,
            reason: "none of the offered revisions has a stateless lifecycle",
        });
    }
    // SEP-2575: a server that does not speak the revision a request
    // names answers `-32022` listing the ones it does, and the client
    // retries with the newest it shares. A server may list the very
    // revision it just refused (a transient rejection, or a balancer
    // in front of mixed deployments), so a revision it names is tried
    // up to twice before it is given up on.
    let mut attempts: Vec<(crate::domain::ProtocolVersion, u8)> =
        stateless.iter().map(|version| (*version, 0)).collect();
    let mut candidate = stateless.first().copied();
    loop {
        let Some(current) = candidate else {
            return Err(ConnectError::Unsupported {
                kind: TransportKind::StatelessHttp,
                reason: "the server shares no stateless revision with this client",
            });
        };
        if let Some((_, tried)) = attempts.iter_mut().find(|(version, _)| *version == current) {
            *tried = tried.saturating_add(1);
        }
        let config = ClientConfig::speaking(vec![current], TransportKind::StatelessHttp)?
            .with_capabilities(capabilities);
        let capabilities_json =
            serde_json::Value::Object(super::handshake::client_capabilities(&config));
        let mut client = StatelessHttpClient::new(
            HttpClientConfig::new(spec.target())?,
            current,
            capabilities_json,
        )?;
        if let Some(auth) = options.auth.clone() {
            client = client.authorizing(auth);
        }
        let transport: Arc<dyn ClientTransport> = Arc::new(client);
        match handshake(transport.as_ref(), &config).await {
            Ok(session) => {
                return Ok((
                    Client::from_session(&session, Arc::clone(&transport)),
                    transport,
                ));
            }
            Err(HandshakeError::Transport(refused)) if refused.supported_versions().is_some() => {
                let supported = refused.supported_versions().unwrap_or_default();
                candidate = attempts
                    .iter()
                    .find(|(version, tried)| {
                        *tried < 2 && supported.iter().any(|name| *name == version.as_wire())
                    })
                    .map(|(version, _)| *version);
            }
            Err(other) => return Err(other.into()),
        }
    }
}

/// Why `kind` has no client adapter here, in the words of someone who has to
/// decide what to do about it.
///
/// Split from the dispatch above so the explanation and the match cannot drift:
/// the arm that refuses is the arm that knows why. Always compiled (not gated
/// on `http` or `legacy-sse`) so the no-adapter `open` above can name the
/// specific kind it is refusing — a stdio spec in a no-features build still
/// gets the "no command line" reason, not a generic "no adapter" string.
const fn missing_reason(kind: TransportKind) -> &'static str {
    match kind {
        TransportKind::Stdio => {
            "a stdio upstream needs a program to run, and a spec carries no \
             command line"
        }
        TransportKind::StatelessHttp => {
            "the stateless HTTP client is compiled in with the `http` feature"
        }
        // Reached only when `legacy-sse` is off while `http` is on, which is
        // the one build where a kind exists in the table and has no adapter.
        _ => "no client adapter for this transport is compiled in",
    }
}

/// The revisions a client offers when the caller expresses no preference.
///
/// Every revision this build has wire types for. Narrowing is the caller's
/// call — a client pinned to one revision is useful, and so is one that will
/// speak whatever the server prefers — but the *default* should not silently
/// be a subset, because a client that offers less than it can speak is a
/// client that gets refused by servers it would have worked with.
///
/// # Errors
///
/// [`SupportedVersionsError`] only if this build's own table were empty, which
/// [`SupportedVersions::all`] documents cannot happen. Returned rather than
/// unwrapped because a function that cannot fail should say so in its type, and
/// because inventing a one-revision fallback here would silently narrow a
/// client on a build whose table is broken — which is the exact failure the
/// default exists to prevent.
pub fn default_versions() -> Result<SupportedVersions, SupportedVersionsError> {
    SupportedVersions::all()
}

#[cfg(test)]
mod tests {
    use crate::domain::{ProtocolVersion, TransportKind, TransportSpec};

    use super::{ConnectError, default_versions, open};

    /// Anything a test here can fail at.
    #[derive(Debug)]
    enum Failure {
        /// A test assumption about the crate's own tables did not hold.
        Expectation(&'static str),
        /// Opening a session failed in a way the test did not expect.
        Connect(ConnectError),
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Expectation(what) => write!(formatter, "expected {what}"),
                Self::Connect(error) => write!(formatter, "connecting failed: {error}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<ConnectError> for Failure {
        fn from(error: ConnectError) -> Self {
            Self::Connect(error)
        }
    }

    impl From<crate::domain::SupportedVersionsError> for Failure {
        fn from(error: crate::domain::SupportedVersionsError) -> Self {
            Self::Expectation(match error {
                crate::domain::SupportedVersionsError::Empty => "a non-empty default",
            })
        }
    }

    /// A spec of `kind` pointing somewhere that is never dialled, because these
    /// tests read the refusal before any socket is opened.
    fn spec(kind: TransportKind) -> Result<TransportSpec, Failure> {
        TransportSpec::new(kind, "http://127.0.0.1:1/mcp")
            .map_err(|_| Failure::Expectation("a usable spec"))
    }

    /// A transport with no client adapter is refused by name, before dialling.
    ///
    /// The refusal has to name the kind. A caller told only "could not connect"
    /// retries a wrong URL forever, and the kind is the one fact that tells them
    /// to build with a different feature instead.
    #[cfg(not(feature = "http"))]
    #[tokio::test]
    async fn a_kind_with_no_adapter_is_refused_by_name() -> Result<(), Failure> {
        let refused = open(&spec(TransportKind::StatelessHttp)?, &default_versions()?).await;
        match refused {
            Err(ConnectError::Unsupported { kind, reason }) => {
                if kind != TransportKind::StatelessHttp {
                    return Err(Failure::Expectation("the refused kind reported back"));
                }
                if reason.is_empty() {
                    return Err(Failure::Expectation("a reason that says what to do"));
                }
                Ok(())
            }
            Err(other) => Err(Failure::Connect(other)),
            Ok(_) => Err(Failure::Expectation("a refusal rather than an adapter")),
        }
    }

    /// A stateless spec offered only handshake revisions is refused before
    /// dialling: there is nothing it could speak statelessly.
    #[cfg(feature = "http")]
    #[tokio::test]
    async fn a_stateless_spec_with_no_stateless_revision_is_refused() -> Result<(), Failure> {
        let versions = crate::domain::SupportedVersions::from_slice(&[
            crate::domain::ProtocolVersion::V2025_11_25,
        ])?;
        match open(&spec(TransportKind::StatelessHttp)?, &versions).await {
            Err(ConnectError::Unsupported {
                kind: TransportKind::StatelessHttp,
                ..
            }) => Ok(()),
            Err(other) => Err(Failure::Connect(other)),
            Ok(_) => Err(Failure::Expectation("a refusal")),
        }
    }

    /// A `stdio` spec is refused, and the reason names the missing thing.
    ///
    /// `TransportSpec` parses a `stdio` spec happily — `kind:target` is a
    /// string. What it cannot carry is the command line a stdio upstream needs,
    /// so the honest answer is a refusal whose reason says so, rather than a
    /// client that connects to a filename.
    ///
    /// This and [`a_kind_with_no_adapter_is_refused_by_name`] are the same call
    /// with two kinds, kept apart because the reasons they assert on differ: one
    /// is about *why this kind*, the other about *which kind came back*.
    #[tokio::test]
    async fn a_stdio_spec_is_refused_for_wanting_a_command_line() -> Result<(), Failure> {
        match open(&spec(TransportKind::Stdio)?, &default_versions()?).await {
            Err(ConnectError::Unsupported { reason, .. }) => {
                if !reason.contains("command") {
                    return Err(Failure::Expectation(
                        "a reason naming the command line that is missing",
                    ));
                }
                Ok(())
            }
            Err(other) => Err(Failure::Connect(other)),
            Ok(_) => Err(Failure::Expectation("a refusal rather than a session")),
        }
    }

    /// The default offer is every revision the build knows, not a subset.
    ///
    /// A default that quietly offered one revision would connect to old servers
    /// and refuse new ones, and the caller would have no way to see that the
    /// narrowing happened.
    #[test]
    fn the_default_offers_every_known_revision() -> Result<(), Failure> {
        let offered = default_versions()?;
        if offered.len() != ProtocolVersion::KNOWN.len() {
            return Err(Failure::Expectation(
                "the default to offer every revision with wire types",
            ));
        }
        for known in ProtocolVersion::KNOWN {
            if !offered.contains(&known) {
                return Err(Failure::Expectation("every known revision to be offered"));
            }
        }
        Ok(())
    }
}
