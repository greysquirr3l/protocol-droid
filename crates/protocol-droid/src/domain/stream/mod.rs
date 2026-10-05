//! Streams that a client can fall behind and then catch up on.
//!
//! # Why this is not `domain/mcp`
//!
//! The replay ring knows nothing about MCP. It holds opaque payloads, numbers
//! them, forgets old ones and reports when it has forgotten. `Last-Event-ID` is
//! the HTTP name for asking it a question, and A2A over server-sent events would
//! ask the same one — so the state machine is here, under a name that does not
//! commit the protocol, and the MCP-specific use of it stays in the transport
//! adapter. That is the same split the JSON-RPC core uses: the envelope and the
//! session machinery are protocol-neutral, and the MCP types sit in `mcp`
//! submodules above them.
//!
//! # What the specification actually requires of a resumable stream
//!
//! Two things, and the second is the one that is easy to get wrong.
//!
//! Events must be identifiable so a client can say where it stopped, and a
//! client that reconnects must not receive a duplicate or a gap *without being
//! told*. The `2025-11-25` revision of the Streamable HTTP transport is the last
//! one that has this: `2026-07-28` removes it outright (major change 9,
//! SEP-2575, `crates/protocol-droid-schema/spec/2026-07-28/CHANGELOG.md` —
//! "Remove SSE stream resumability and message redelivery (the `Last-Event-ID`
//! header and SSE event IDs) ... clients **MUST** re-issue it as a new request
//! with a new request ID"). So this ring serves every revision up to and
//! including `2025-11-25`, and on a `2026-07-28` session nothing should consult
//! it at all.
//!
//! The gap requirement is why [`Replay`] is an enum with four arms rather than a
//! `Vec`. A bounded ring has to forget, and a forgotten event that the client
//! never hears about is a lost event reported as a success. Returning an empty
//! slice for both "you are up to date" and "I dropped three events you never
//! saw" is the failure this type exists to make impossible to write.
//!
//! # Why the capacity is the caller's to choose
//!
//! Because the ring cannot pick it correctly on the caller's behalf. What a
//! bound trades is memory against how long a reconnect may last, and the memory
//! side is set by the size of the payload — an adapter streaming progress
//! notifications and an adapter streaming `resources/read` results have no
//! common answer, and the specification fixes none. So [`EventRing::new`]
//! demands a capacity rather than carrying a default, and
//! [`RECOMMENDED_CAPACITY`] is offered only as a starting point for an adapter
//! that has no better number. `recommended_capacity_covers_a_reconnect` is
//! where the reasoning behind that number is written down and checked.

mod event_name;
mod event_ring;

pub use event_name::{EventName, EventNameError, names};
pub use event_ring::{
    EventId, EventRing, RECOMMENDED_CAPACITY, RecommendedCapacity, Replay, StreamEvent,
};
