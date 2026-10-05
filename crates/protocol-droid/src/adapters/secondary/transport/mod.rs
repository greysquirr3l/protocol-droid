//! Driven transports: the clients the core calls out to.
//!
//! Split from [`crate::adapters::secondary`] by the axis that matters here.
//! `codec`, `idempotency` and `mcp` are in-process machinery — they speak to
//! the same process over shared memory. This subtree talks to a peer over a
//! socket, and everything under it is gated together because a consumer that
//! cannot open a socket has no use for any of it.
//!
//! The stdio client lives one level up, under `secondary::stdio`, gated by its
//! own feature. It is the same category of thing with a different dependency:
//! pipes and a child process rather than TLS and a hyper client, and the two
//! are alternatives rather than a set.
//!
//! [`detect`] sits above both HTTP clients and is deliberately not a method on
//! either. It is the step that runs *before* anyone knows which transport the
//! peer speaks, so it cannot belong to one of them: the request that reveals the
//! peer's transport is itself sent by neither.

#[cfg(feature = "http")]
pub mod detect;
#[cfg(all(test, feature = "http"))]
mod test_support;

#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "legacy-sse")]
pub mod legacy_sse;
