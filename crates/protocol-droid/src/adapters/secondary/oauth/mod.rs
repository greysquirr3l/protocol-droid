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
