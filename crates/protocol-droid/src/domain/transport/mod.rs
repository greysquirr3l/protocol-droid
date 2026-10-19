//! `TransportKind` — the four transports the specification has ever defined,
//! and which revision defines each.
//!
//! # Where the boundaries come from
//!
//! Every entry below cites the vendored changelog that moved the boundary,
//! because these are the kind of facts that are wrong in the model's memory and
//! right in the repository:
//!
//! | Transport            | Defined for       | Source |
//! | -------------------- | ----------------- | ------ |
//! | [`Stdio`]            | every revision    | The first revision defines it; `2025-11-25` clarifies that servers may log to stderr, and `2026-07-28` still specifies `server/discover` "on STDIO" |
//! | [`LegacySse`]        | `2024-11-05` only | `2025-03-26` changelog: "Replaced the previous HTTP+SSE transport with a more flexible Streamable HTTP transport" |
//! | [`StreamableHttp`]   | `2025-03-26` onward | Same changelog entry; `2026-07-28` then removes protocol-level sessions and SSE resumability from it |
//! | [`StatelessHttp`]    | `2026-07-28` onward | `2026-07-28` changelog: "Make MCP stateless: remove the `initialize`/`notifications/initialized` handshake" |
//!
//! [`Stdio`]: Self::Stdio
//! [`LegacySse`]: Self::LegacySse
//! [`StreamableHttp`]: Self::StreamableHttp
//! [`StatelessHttp`]: Self::StatelessHttp
//!
//! # What else is here
//!
//! [`framing`] — the byte-level delimiter, which is not an MCP concept and
//! carries no revision. It lives beside `TransportKind` rather than under an
//! adapter because the stdio server and the stdio client are *both* its
//! consumers, and a shared piece of a transport that only one of the two can
//! reach is a shared piece of a transport that one of them has to fork.

// Private, like this module: the items are re-exported by name rather than the
// module itself, so `unreachable_pub` does not apply and there is exactly one
// path to each of them.
mod endpoint;
mod framing;
mod rate_limit;
mod transport_spec;

pub use endpoint::{MessageEndpoint, MessageEndpointError};
pub use framing::{FrameDecoder, FramingError, MaxFrameSize, MaxFrameSizeError, encode_frame};
pub use rate_limit::{RateLimit, Verdict, check as check_rate_limit};
pub use transport_spec::{SpecError, TransportSpec};

use core::fmt;

use crate::domain::ProtocolVersion;

/// A transport the specification defines.
///
/// Ordered by the revision that introduced it, so the declaration order is
/// chronological and `Ord` is derived rather than written. See
/// `tests::derived_ordering_agrees_in_both_directions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TransportKind {
    /// Newline-delimited JSON over a child process's standard streams.
    ///
    /// Defined by every revision, `2024-11-05` through the newest. It predates
    /// the HTTP transports and is not a lifecycle the protocol version changes.
    Stdio,
    /// HTTP+SSE as defined for `2024-11-05`, where the client opened a GET
    /// stream and the server held a single POST endpoint.
    ///
    /// Removed in `2025-03-26`. Kept as a distinct variant rather than folded
    /// into [`StreamableHttp`] because a bridge that receives a message from
    /// one of these has to be able to say so, and because it is the only
    /// revision that can require this branch to exist.
    ///
    /// [`StreamableHttp`]: Self::StreamableHttp
    LegacySse,
    /// The stateful Streamable HTTP transport introduced in `2025-03-26`: a
    /// single endpoint that upgrades to SSE, with an `Mcp-Session-Id` header
    /// tying calls together.
    StreamableHttp,
    /// The stateless lifecycle introduced in `2026-07-28`.
    ///
    /// Distinct from [`StreamableHttp`] rather than a mode of it, because the
    /// two differ in what the session tracks: this one has no protocol-level
    /// session at all, every request carries its version and capabilities in
    /// `_meta`, and there is no `initialize` handshake. Code that caches per
    /// session cannot treat them alike.
    ///
    /// [`StreamableHttp`]: Self::StreamableHttp
    StatelessHttp,
}

impl TransportKind {
    /// Every transport, in the order the revisions introduced them.
    pub const ALL: [Self; 4] = [
        Self::Stdio,
        Self::LegacySse,
        Self::StreamableHttp,
        Self::StatelessHttp,
    ];

    /// The transport's short name, for logs, errors and CLI arguments.
    ///
    /// These are this project's own vocabulary rather than the specification's
    /// — the spec never names its transports in a versioned field — so
    /// `StreamableHttp` is `http` and `StatelessHttp` is `http-stateless`. The
    /// CLI surfaces the same spellings, and changing one means changing both.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::LegacySse => "sse",
            Self::StreamableHttp => "http",
            Self::StatelessHttp => "http-stateless",
        }
    }
}

impl fmt::Display for TransportKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Whether the specification defines `transport` for `version`.
///
/// A pure predicate over the two value objects: no session, no negotiation, no
/// I/O. T06 consults it when deciding what a negotiated pair can be run over,
/// and the adapters consult it when deciding whether a transport is even
/// registerable.
///
/// # Unknown revisions deny
///
/// [`ProtocolVersion::Unknown`] returns `false` for every transport, and that
/// is the load-bearing decision rather than an omission. This build has no wire
/// types for an unknown revision, so nothing can be lowered to it, and the
/// project's posture is fail-secure default deny. The tempting alternative —
/// assuming a future revision keeps the transports its predecessors had — is
/// exactly the confident wrong the crate exists to avoid: a revision is free to
/// remove a transport, and a client that assumed otherwise would speak a
/// transport the peer had deleted.
///
/// A peer that offers only unknown revisions is not left hanging, either:
/// [`SupportedVersions::best_match`](crate::domain::SupportedVersions::best_match) picks the newest revision both sides
/// actually share, and JSON-RPC has an error code for there being none.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{ProtocolVersion, TransportKind, transport_allowed};
///
/// assert!(transport_allowed(ProtocolVersion::V2024_11_05, TransportKind::LegacySse));
/// assert!(!transport_allowed(ProtocolVersion::V2025_03_26, TransportKind::LegacySse));
/// assert!(transport_allowed(ProtocolVersion::V2026_07_28, TransportKind::StatelessHttp));
/// assert!(!transport_allowed(ProtocolVersion::V2025_11_25, TransportKind::StatelessHttp));
/// ```
#[must_use]
pub const fn transport_allowed(version: ProtocolVersion, transport: TransportKind) -> bool {
    match version {
        // Every revision defines stdio; nothing in any changelog removes it.
        ProtocolVersion::V2024_11_05 => {
            matches!(transport, TransportKind::Stdio | TransportKind::LegacySse)
        }
        ProtocolVersion::V2025_03_26
        | ProtocolVersion::V2025_06_18
        | ProtocolVersion::V2025_11_25 => {
            matches!(
                transport,
                TransportKind::Stdio | TransportKind::StreamableHttp
            )
        }
        ProtocolVersion::V2026_07_28 => matches!(
            transport,
            TransportKind::Stdio | TransportKind::StreamableHttp | TransportKind::StatelessHttp
        ),
        // No wire types for an unknown revision, so nothing can be served.
        ProtocolVersion::Unknown(_) => false,
    }
}

#[cfg(test)]
mod tests {
    // Test code unwraps deliberately: the strict profile exists to keep
    // production paths fallible, and a test that cannot unwrap cannot assert.
    // The messages name the failing precondition, which is the useful part.
    #![expect(
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
    )]

    use super::{TransportKind, transport_allowed};
    use crate::domain::{ProtocolVersion, ReleaseDate};

    /// The 1.98 trap: `derive(PartialOrd)` has a fast path that assumes a
    /// hand-written `PartialOrd` agrees with a derived `Ord`. A pair that
    /// disagrees silently yields wrong comparisons instead of a compile error.
    /// Every ordered type in this crate derives both traits from the same
    /// variant order and copies this test when it is added.
    #[test]
    fn derived_ordering_agrees_in_both_directions() {
        for left in TransportKind::ALL {
            for right in TransportKind::ALL {
                let total = left.cmp(&right);
                assert_eq!(left.partial_cmp(&right), Some(total));
                assert_eq!(left < right, total.is_lt());
                assert_eq!(left > right, total.is_gt());
            }
        }
        assert!(TransportKind::Stdio < TransportKind::LegacySse);
        assert!(TransportKind::LegacySse < TransportKind::StreamableHttp);
        assert!(TransportKind::StreamableHttp < TransportKind::StatelessHttp);
    }

    /// The exhaustive table the task asks for: every `(version, transport)`
    /// pair, asserted. Written as a table rather than as a pile of `assert!`
    /// calls so that adding a transport or a revision shows up as a diff that
    /// has to be answered, instead of as a silently untested pair.
    #[test]
    fn the_whole_matrix_is_what_the_changelogs_say() {
        //  Stdio  SSE  Streamable  Stateless
        let expected: [[bool; 4]; 5] = [
            // 2024-11-05 — stdio and the legacy HTTP+SSE transport.
            [true, true, false, false],
            // 2025-03-26 — HTTP+SSE replaced by Streamable HTTP.
            [true, false, true, false],
            // 2025-06-18 — unchanged.
            [true, false, true, false],
            // 2025-11-25 — unchanged.
            [true, false, true, false],
            // 2026-07-28 — the stateless lifecycle arrives.
            [true, false, true, true],
        ];
        for (version, row) in ProtocolVersion::KNOWN.iter().zip(expected) {
            for (transport, allowed) in TransportKind::ALL.iter().zip(row) {
                assert_eq!(
                    transport_allowed(*version, *transport),
                    allowed,
                    "{version} over {transport}"
                );
            }
        }
    }

    /// The fail-secure decision, asserted from both sides: nothing is allowed,
    /// so nothing has to be argued about per transport.
    #[test]
    fn unknown_revisions_allow_nothing() {
        for date in ["2027-01-01", "2030-12-31", "2999-01-01"] {
            let version = ProtocolVersion::Unknown(date.parse::<ReleaseDate>().unwrap());
            for transport in TransportKind::ALL {
                assert!(
                    !transport_allowed(version, transport),
                    "{version} must not allow {transport}"
                );
            }
        }
    }

    #[test]
    fn stdio_is_defined_for_every_known_revision() {
        for version in ProtocolVersion::KNOWN {
            assert!(
                transport_allowed(version, TransportKind::Stdio),
                "{version}"
            );
        }
    }

    #[test]
    fn short_names_are_stable_and_distinct() {
        let names: Vec<&str> = TransportKind::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(names, vec!["stdio", "sse", "http", "http-stateless"]);
        let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "short names collide");
        for transport in TransportKind::ALL {
            assert_eq!(transport.to_string(), transport.as_str());
        }
    }
}
