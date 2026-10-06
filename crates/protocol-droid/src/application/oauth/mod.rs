//! OAuth 2.1 authorization, as MCP's revisions specify it.
//!
//! Which revision requires what is [`authorization_model`](crate::domain::authorization_model)
//! in `domain/`. This module holds the decisions that follow from it, and they
//! are split by who makes them: [`token`] is the pure logic this crate owns and
//! can be tested without a socket, while the crypto and HTTP that turn a
//! presented string into claims live in adapters.
//!
//! No scope vocabulary is defined here. The vendored spec names no scope
//! strings, so any table would be this crate inventing one and presenting it as
//! policy — see [`token::ScopePolicy`].

// `client` reads `domain::oauth`, which is gated on the same feature, so it
// carries the gate too. `token` does not and stays ungated: it is pure claim
// logic with no dependency on a SHA-256 implementation or a base64url codec.
#[cfg(feature = "oauth")]
pub mod client;
mod token;

pub use token::{
    NoScopes, Refusal, ScopePolicy, SourceError, TokenClaims, TokenPolicy, TokenSource,
    authenticate, verify,
};
