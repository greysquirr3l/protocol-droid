//! The network server transports, one module per transport.
//!
//! Split from [`super::stdio`](super::stdio) rather than folded into it because
//! the two share almost nothing. stdio owns a pair of byte streams and a
//! framing ceiling; Streamable HTTP owns a URL space, a header block, status
//! codes and a resumable event log. What they do share — the domain's
//! `FrameDecoder`, the session store, the `PeerSession` the dispatcher needs —
//! is shared because it is already below the transport, not because it lives
//! here.
//!
//! T15 adds a `legacy_sse` module beside `http`, not inside it. (Named in prose
//! rather than as an intra-doc link, because the module does not exist yet and
//! a link check would fail on a forward reference.) The 2024-11-05 HTTP+SSE
//! transport is a different wire protocol that happens to run over HTTP;
//! sharing a router with it would couple two lifecycles that the specification
//! keeps separate.

#[cfg(feature = "http")]
pub mod http;
