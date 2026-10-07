//! The front door a consumer walks through.
//!
//! Everything here delegates. The point is that a consumer does not have to
//! know that a [`Dispatcher`] exists, that a [`PeerSession`] needs a
//! [`FeatureProfile`] resolved before the loop starts, or that
//! [`ServeLoop::new`] takes seven arguments — none of which is a decision a
//! consumer of this crate should have to make.
//!
//! The one thing deliberately *not* hidden is the session's ceiling. A profile
//! is resolved from a revision, and until a peer says which revision it wants,
//! something has to say what the session permits before the first frame. The
//! [`StdioBuilder::version`] ceiling answers that, and it defaults to the
//! newest revision this build can complete a handshake on rather than to a
//! constant a consumer has to pick and remember to revisit.

use std::sync::Arc;

use crate::adapters::primary::stdio::{Diagnostics, ServeLoop, StdioConfig};
use crate::application::jsonrpc::ProfileBatching;
use crate::application::server::Server;
use crate::application::server::context::{DynExchange, ExchangeError, PeerSession};
use crate::application::server::dispatch::Dispatcher;
use crate::application::session::SinkError;
use crate::application::session::frame_sink::FrameSink;
use crate::domain::feature::resolve;
use crate::domain::ir::JsonObject;
use crate::domain::session::SessionId;
use crate::domain::{
    CapabilitySet, Negotiation, ProtocolVersion, SupportedVersions, TransportKind,
};

/// Builds a stdio server and serves it on this process's stdin and stdout.
///
/// The one-call form is [`Server::serve_stdio`]; this type exists for the case
/// where the caller needs to change one of the two things that are genuinely
/// decisions — the revision ceiling, or the framing limits.
///
/// [`Server::serve_stdio`]: crate::application::server::Server::serve_stdio
///
/// # Examples
///
/// ```no_run
/// # use protocol_droid::application::server::{Server, serve::StdioBuilder};
/// # use protocol_droid::domain::ProtocolVersion;
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let mut loop_ = StdioBuilder::new(Server::builder("greeter").build())
///     .version(ProtocolVersion::V2025_06_18)
///     .build();
/// let report = loop_.serve().await?;
/// eprintln!("{report:?}");
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct StdioBuilder {
    server: Server,
    version: ProtocolVersion,
    config: StdioConfig,
}

impl StdioBuilder {
    /// A builder serving `server` over stdio.
    #[must_use]
    pub fn new(server: Server) -> Self {
        Self {
            server,
            version: default_ceiling(),
            config: StdioConfig::default(),
        }
    }

    /// The newest revision this session is permitted to use before a peer
    /// says otherwise.
    ///
    /// This is a **ceiling, not an answer**. The revision a peer is told in
    /// its handshake is settled from its own `initialize`; this only decides
    /// what the session gates on in the moment before that, and what it falls
    /// back to for a peer that never sent one.
    ///
    /// Defaults to the newest revision with a handshake defined. The obvious
    /// alternative — the newest revision the crate knows — is `2026-07-28`,
    /// which SEP-2575 removed `initialize` from, so a server that chose it
    /// would gate its whole life on a revision no peer can complete.
    #[must_use]
    pub const fn version(mut self, version: ProtocolVersion) -> Self {
        self.version = version;
        self
    }

    /// The framing limits: frame ceiling and diagnostics writer.
    #[must_use]
    pub const fn config(mut self, config: StdioConfig) -> Self {
        self.config = config;
        self
    }

    /// Assembles the loop this builder describes.
    ///
    /// Infallible: everything it needs is either in `self` or derived from it
    /// by [`resolve`], which is a pure function over a [`Negotiation`] this
    /// builder constructs itself.
    #[must_use]
    pub fn build(
        self,
    ) -> ServeLoop<tokio::io::Stdin, tokio::io::Stdout, tokio::io::Stderr, ProfileBatching> {
        let profile = resolve(Negotiation {
            version: self.version,
            client_caps: CapabilitySet::empty(),
            server_caps: CapabilitySet::all_for(self.version),
            transport: TransportKind::Stdio,
        });
        let peer = PeerSession::new(
            self.version,
            profile,
            SessionId::new(1),
            TransportKind::Stdio,
            Arc::new(NoCallbacks),
            Arc::new(NullSink),
        );
        ServeLoop::new(
            tokio::io::stdin(),
            tokio::io::stdout(),
            Diagnostics::stderr(),
            self.config,
            Arc::new(Dispatcher::new(Arc::new(self.server))),
            peer,
            ProfileBatching,
        )
    }
}

/// The newest revision this build can complete a handshake on.
///
/// Falls back to [`ProtocolVersion::latest_known`] if that set is somehow
/// empty, leaving the ceiling a question rather than a wrong answer.
fn default_ceiling() -> ProtocolVersion {
    SupportedVersions::all().map_or_else(
        |_| ProtocolVersion::latest_known(),
        |supported| supported.latest_handshake(),
    )
}

/// Where a server-initiated request would be written, if one were ever made.
///
/// A `PeerSession` needs somewhere to put them and the stdio loop wires no
/// callback exchange, so this refuses every send rather than pretending to
/// deliver. `Inert` rather than a real stream so nothing a test asserts about
/// ordering is really an assertion about two writers racing.
#[derive(Debug, Clone, Copy)]
struct NullSink;

impl FrameSink for NullSink {
    /// Always refuses: nothing in this path is ever written.
    ///
    /// # Errors
    ///
    /// Always a [`SinkError`], naming the absent sink.
    fn send(&self, _frame: &str) -> impl std::future::Future<Output = Result<(), SinkError>> {
        std::future::ready(Err(SinkError))
    }
}

/// An exchange that services nothing.
#[derive(Debug, Clone, Copy)]
struct NoCallbacks;

impl DynExchange for NoCallbacks {
    /// Always refuses: the stdio loop wires no callbacks.
    ///
    /// # Errors
    ///
    /// Always an [`ExchangeError`], naming the absent exchange.
    fn exchange_boxed<'a>(
        &'a self,
        _method: &'a str,
        _params: &'a JsonObject,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<JsonObject, ExchangeError>> + Send + 'a>,
    > {
        Box::pin(std::future::ready(Err(ExchangeError::new(
            "this server wires no callback exchange",
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default ceiling is a revision a handshake can complete on.
    ///
    /// The obvious wrong answer is `latest_known()`, which is `2026-07-28` —
    /// the revision SEP-2575 removed `initialize` from. A server that gated its
    /// whole life on it would answer every peer it could not finish talking to.
    #[test]
    fn the_default_ceiling_has_a_handshake() {
        let ceiling = default_ceiling();
        assert!(
            crate::domain::handshake_defined_in(ceiling),
            "{ceiling} has no initialize on it"
        );
    }

    /// The ceiling is *not* the newest revision the crate knows.
    ///
    /// Those two differ by exactly the revisions that dropped a handshake, and
    /// `2026-07-28` is currently one of them. Were they ever to converge, a
    /// revision would have re-added `initialize` and this assertion would be
    /// the signal to look at the ceiling's meaning again.
    #[test]
    fn the_ceiling_is_not_merely_the_newest_known() {
        assert_ne!(default_ceiling(), ProtocolVersion::latest_known());
    }

    /// The loop this builder describes gates on the ceiling, not on a
    /// revision someone picked.
    #[test]
    fn the_built_session_gates_on_the_ceiling() {
        let server = Server::builder("ceiling-test").build();
        let loop_ = StdioBuilder::new(server)
            .version(ProtocolVersion::V2024_11_05)
            .build();
        assert!(
            !loop_
                .peer()
                .profile()
                .allows(crate::domain::Feature::Completions),
            "a 2024-11-05 session must not carry a 2025-03-26 feature"
        );
    }
}
