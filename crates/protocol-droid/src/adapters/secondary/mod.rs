//! Driven adapters: the client transports and the injected side-effect ports.
//!
//! Populated by T13 (stdio, including child-process spawning), T14 and T15
//! (client-side HTTP transports) and T10 (the `Clock` and `IdSource` ports the
//! domain depends on but must never call globally).

pub mod codec;
pub mod idempotency;
pub mod mcp;
// T14/T15. The client half of the HTTP transports. Gated because it is the one
// driven adapter that opens a socket to somewhere that is not this process: a
// consumer that only embeds the IR and the codecs has no use for a TLS stack,
// and `default = false` is how it says so.
#[cfg(feature = "http")]
pub mod oauth;

#[cfg(feature = "http")]
pub mod transport;

// T13. The client half of the stdio transport: spawning a child server, killing
// it on drop, and keeping its stderr off the protocol stream. Gated with the
// server half because it is the same dependency — a tokio runtime and pipes —
// and a build that speaks stdio on one side and not the other has no use.
#[cfg(feature = "stdio")]
pub mod stdio;
