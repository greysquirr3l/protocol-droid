//! OAuth 2.1 adapters: driven, because each one reaches an authorization server
//! that lives somewhere else.
//!
//! One per mechanism for establishing *authenticity*. Which revision requires
//! any of it, and what a verified token is then allowed to do, are both in
//! `application/oauth/`.

pub mod introspection;
