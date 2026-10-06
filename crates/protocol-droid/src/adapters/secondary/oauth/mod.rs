//! OAuth 2.1 adapters: driven, because each one reaches an authorization server
//! that lives somewhere else.
//!
//! One per mechanism for establishing *authenticity*. Which revision requires
//! any of it, and what a verified token is then allowed to do, are both in
//! `application/oauth/`.
//!
//! The two are interchangeable behind
//! [`TokenSource`](crate::application::oauth::TokenSource), and a deployment
//! picks between them on cost rather than on trust: [`introspection`] asks the
//! authorization server about every
//! request and is right the instant a token is revoked, while [`jwt`] checks
//! the signature locally and is right only until the token expires or its key
//! is withdrawn.

pub mod introspection;
pub mod jwt;

// T16. The client half's secret source. Gated on `random-ids` as well as on
// `oauth`, because it needs both the port (which only exists when `oauth` is on)
// and an entropy source. A consumer that wants its own — a hardware token, a KMS
// — links `oauth` without `random-ids` and implements the port itself, which is
// the whole reason the port exists.
#[cfg(feature = "random-ids")]
pub mod secrets;
// T16. RFC 7591 dynamic registration and Client ID Metadata Document fetch.
// `oauth` already gates this module, so no second gate here.
pub mod registration;
