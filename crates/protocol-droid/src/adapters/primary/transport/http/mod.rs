//! The Streamable HTTP server adapter: everything an axum server is told,
//! shown, or asked before a single byte of protocol is dispatched.
//!
//! # What is here and what is not
//!
//! This module is the *configuration and refusal* half of T14. It owns three
//! things, and each is a value rather than a function because each of them has
//! to be the same value in three places — the router, the session store, and
//! the SSE responder — and three separately assembled answers is three ways for
//! them to disagree:
//!
//! - [`ServerConfig`]: the endpoint path, the body ceiling, the replay window,
//!   the streaming policy, the timeouts, the security headers, and the scopes
//!   a caller must hold.
//! - [`SecurityHeaders`]: the three response headers the project's security
//!   rules require, with strict defaults.
//! - [`Refusal`]: the status code and JSON-RPC error body for every request the
//!   adapter declines to answer, in one exhaustive `match`.
//!
//! The [`Authenticator`] port and the [`AuthPolicy`] that consumes it sit
//! beside them: the port is declared by the consumer (which is why it lives
//! here, in the adapter that will call it, rather than in a `ports` module that
//! does not exist in this layout), and T16 supplies the OAuth implementation.
//!
//! **What is deliberately absent is the router itself.** There is no `Router`,
//! no route table, no session store binding and no SSE responder in this module,
//! and that is the next dispatch rather than an oversight: a router assembled
//! before the session store has a shape to bind to would be a router that gets
//! rewritten. What lands here instead is the vocabulary that router will need
//! — [`Endpoint`] to mount on, [`ServerConfig`] to read, [`Refusal`] to answer
//! with, and [`AuthPolicy`] to decide with — so that when it arrives it is an
//! assembly job rather than a design job.
//!
//! # The fail-secure shape
//!
//! Two independent things make an unconfigured deployment fail-secure rather
//! than fail-open, and they are worth separating because they fail differently:
//!
//! - **Structure.** Every stateful path resolves an [`AuthContext`] before it
//!   looks at a session, a body, or a stream. There is no route that skips it,
//!   because a session is reachable only through the context it was handed.
//! - **Policy.** [`AuthContext::anonymous`] holds no scopes, and
//!   [`AuthPolicy::refuse`] compares the caller's scopes against
//!   [`ServerConfig::required_scopes`] *before* anything else happens. Name a
//!   scope and every anonymous caller is refused `401`; a caller with a
//!   principal that lacks a named scope is refused `403`.
//!
//! The default configuration names no scopes, and that default is a decision
//! with a stated cost rather than an absence: it serves a read-only endpoint to
//! anyone who can reach it. It is the right default for a loopback transport
//! and a conformance runner, and the wrong one for a routable deployment — which
//! is precisely why [`AuthPolicy::Require`] and
//! [`ServerConfig::with_required_scopes`] exist and why an empty
//! [`AuthPolicy::Public`] is a thing a deployment writes on purpose.
//!
//! # Example
//!
//! ```
//! use protocol_droid::adapters::primary::transport::http::{ServerConfig, SecurityHeaders};
//!
//! // The specification's own endpoint, the domain's frame ceiling, and the
//! // strict security headers — the defaults a public deployment runs.
//! let config = ServerConfig::new()?;
//! assert_eq!(config.endpoint().as_str(), "/mcp");
//! assert!(config.security().content_security_policy().contains("default-src 'none'"));
//!
//! // A deployment that decides this endpoint needs a credential says so.
//! use protocol_droid::domain::registry::{Scope, ScopeSet};
//! let locked = config.with_required_scopes(ScopeSet::empty().with(Scope::Read));
//! assert!(locked.required_scopes().contains(Scope::Read));
//!
//! // And one that also relaxes framing, for a deployment that has measured why.
//! let relaxed = ServerConfig::new()?
//!     .with_security(SecurityHeaders::default().with_x_frame_options("SAMEORIGIN"));
//! assert_eq!(relaxed.security().x_frame_options(), "SAMEORIGIN");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod auth;
mod config;
mod refusal;
mod resume;
mod session_store;
mod sse;

pub use auth::{Anonymous, AuthPolicy, Authenticator, Credentials};
pub use config::{Endpoint, EndpointError, SecurityHeaders, ServerConfig, Streaming};
pub use refusal::Refusal;
pub use resume::{Resume, decide, plan};
pub use session_store::{SessionStore, SessionStoreError};
pub use sse::{SseError, SseEvent};

/// The default replay window is the domain's, and the security defaults are
/// [`SecurityHeaders::default`]. This module re-exports nothing else, so a
/// change to a re-export list is the only way a caller reaches a private item.
#[cfg(test)]
mod tests {
    use crate::domain::registry::{AuthContext, Scope, ScopeSet};

    use super::{Endpoint, EndpointError, SecurityHeaders, ServerConfig, Streaming};

    type TestError = Box<dyn std::error::Error>;

    /// The re-export list is the module's public surface, and a `pub` item the
    /// crate cannot name from here is one the router will have to reach around.
    ///
    /// `dead_code` is inherited from the lib root and the test harness builds
    /// the lib with `--test`, so this is the assertion that keeps the surface
    /// honest: a variant removed from a re-export fails here rather than
    /// failing a caller several dispatches from now.
    #[test]
    fn the_public_surface_is_nameable_from_here() -> Result<(), TestError> {
        let config = ServerConfig::new()?.with_streaming(Streaming::Json);
        assert_eq!(config.endpoint(), &"/mcp".parse::<Endpoint>()?);
        assert_eq!(SecurityHeaders::default().x_frame_options(), "DENY");
        // The re-exported auth seam is usable from here without naming the
        // private module, which is the whole point of the re-export list.
        assert_eq!(AuthContext::anonymous().scopes(), ScopeSet::empty());
        assert!(ScopeSet::empty().with(Scope::Read).contains(Scope::Read));
        assert!(
            "mcp"
                .parse::<Endpoint>()
                .is_err_and(|error| error == EndpointError::NoLeadingSlash)
        );
        Ok(())
    }
}
