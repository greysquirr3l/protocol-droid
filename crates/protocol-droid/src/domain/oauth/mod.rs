//! OAuth machinery that is not MCP-specific.
//!
//! The version gating — which revision requires what — lives in
//! [`domain::mcp::authorization`](crate::domain::mcp::authorization), because
//! that is MCP's half. What follows from the gate is OAuth's own vocabulary,
//! and it is protocol-neutral: a PKCE verifier is described by RFC 7636 and
//! knows nothing about a tool call.
//!
//! Gated on `oauth` even though [`domain::mcp::authorization`] is not. The
//! authorization *model* is pure data and costs a lean build nothing, so
//! gating it would take away the thing every consumer needs to read a
//! revision's requirements. This module pulls a SHA-256 implementation and a
//! base64url codec, which is a real cost for a consumer that only embeds the IR
//! and the codecs.

pub mod issued_token;
pub mod issuer;
pub mod pkce;
pub mod registration;
pub mod resource;

pub use issued_token::{AccessToken, IssuedToken, RefreshToken, TokenResponseError};
pub use issuer::{AuthorizationResponseError, check_response_issuer};
pub use pkce::{ChallengeMethod, CodeChallenge, CodeVerifier, PkceError};
pub use registration::{
    ClientId, ClientIdError, ClientSecret, RegisteredClient, RegistrationDecision,
    decide_registration,
};
pub use resource::{ResourceBinding, ResourceIndicatorError, ResourceParameters};
