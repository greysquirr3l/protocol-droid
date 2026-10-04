//! Driven adapters: the client transports and the injected side-effect ports.
//!
//! Populated by T13 (stdio, including child-process spawning), T14 and T15
//! (client-side HTTP transports) and T10 (the `Clock` and `IdSource` ports the
//! domain depends on but must never call globally).

pub mod codec;
pub mod idempotency;
pub mod mcp;

// T13. The client half of the stdio transport: spawning a child server, killing
// it on drop, and keeping its stderr off the protocol stream. Gated with the
// server half because it is the same dependency — a tokio runtime and pipes —
// and a build that speaks stdio on one side and not the other has no use.
#[cfg(feature = "stdio")]
pub mod stdio;
