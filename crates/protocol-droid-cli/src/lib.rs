//! The `protocol-droid` command-line adapter, as a library.
//!
//! ## Why there is a library target at all
//!
//! The binary is a thin `main` that parses arguments and dispatches. Everything
//! it can actually do lives here, and that split is not cosmetic:
//!
//! `unreachable_pub` and `redundant_pub_crate` are both deny-level in this
//! project, and in a crate that is *only* a binary they contradict each other —
//! a `pub` item in a bin crate is genuinely unreachable from outside, and a
//! `pub(crate)` one is genuinely redundant. There is no spelling that satisfies
//! both. In a crate that also has a library target, `pub` means what it says
//! and the question does not arise.
//!
//! So `main.rs` holds the argument contract and the dispatch, and this crate
//! holds the behaviour. The alternative — merging the subcommand into
//! `main.rs` to dodge the lint — would trade a real lint conflict for a
//! 500-line file the project's own file-structure rules disallow.

pub mod bridge;
// The A2A leg of `bridge` lives behind the same feature the
// `A2aJsonRpcPeer` is behind. The MCP-only build does not
// compile it.
#[cfg(feature = "a2a")]
pub mod bridge_a2a;
pub mod check;
pub mod conformance;
pub mod conformance_client;
pub mod inspect;
pub mod probe;
// A2A's well-known card lives in the A2A domain, so the A2A probe
// is feature-gated to match.
#[cfg(feature = "a2a")]
pub mod probe_a2a;
// The A2A conformance runner is the third-party-implementer
// entry point: the same vectors the integration test drives
// in-process are what an external server is judged against
// over HTTP. Feature-gated to match the lib's `a2a` module,
// which the runner depends on.
#[cfg(feature = "a2a")]
pub mod a2a_conformance;
pub mod record;
pub mod replay;
// The serving path needs both transports the library speaks as a client. The
// probe in `bridge` does not, and stays available without them.
#[cfg(feature = "bridge")]
pub mod serve;
