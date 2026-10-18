//! Which client-to-server request methods each revision defines.
//!
//! # Why a table and not the feature matrix
//!
//! The feature matrix answers "may this session use behaviour X", and a feature
//! that switched off is still a feature: `ping` has a row because something can
//! depend on it being refused. Three of the methods `2026-07-28` drops are not
//! like that. `logging/setLevel` and `resources/subscribe` were *replaced* —
//! by the per-request `io.modelcontextprotocol/logLevel` key and by
//! `subscriptions/listen` — so the behaviour they carried survives under
//! another name, and a feature row would say the capability is still there.
//! What changed is the method set, and the method set is a schema fact:
//! membership of each revision's `ClientRequest` union.
//!
//! A method a revision does not define is answered `-32601`, and on the
//! `2026-07-28` HTTP transport with `404 Not Found`. Answering it as though it
//! were merely ungated would tell a client the method exists and refused, when
//! the revision it chose has no such method at all.
//!
//! # Sources
//!
//! Each list is the `ClientRequest` union of the vendored `schema.json` for
//! that revision, with one correction: the `2025-03-26` union omits
//! `resources/templates/list` although the same schema defines
//! `ListResourceTemplatesRequest` and its result. The omission is an upstream
//! slip, not a removal — the method exists on both sides of that revision — so
//! the table follows the defined types rather than the union.
//!
//! - <https://modelcontextprotocol.io/specification/2024-11-05/changelog>
//! - <https://modelcontextprotocol.io/specification/2025-11-25/changelog> —
//!   SEP-1686 adds `tasks/get`, `tasks/result`, `tasks/cancel`, `tasks/list`.
//! - <https://modelcontextprotocol.io/specification/2026-07-28/changelog> —
//!   SEP-2575 removes `initialize`, `ping`, `logging/setLevel`,
//!   `resources/subscribe` and `resources/unsubscribe`, and adds
//!   `server/discover` and `subscriptions/listen`; SEP-2663 moves `tasks/*` to
//!   the `io.modelcontextprotocol/tasks` extension.

use crate::domain::ProtocolVersion;

/// The methods every revision through `2025-11-25` shares.
const STATEFUL: &[&str] = &[
    "completion/complete",
    "initialize",
    "logging/setLevel",
    "ping",
    "prompts/get",
    "prompts/list",
    "resources/list",
    "resources/read",
    "resources/subscribe",
    "resources/templates/list",
    "resources/unsubscribe",
    "tools/call",
    "tools/list",
];

/// SEP-1686's task methods, core only in `2025-11-25`.
const TASKS: &[&str] = &["tasks/cancel", "tasks/get", "tasks/list", "tasks/result"];

/// The `2026-07-28` method set, and every later draft's.
const STATELESS: &[&str] = &[
    "completion/complete",
    "prompts/get",
    "prompts/list",
    "resources/list",
    "resources/read",
    "resources/templates/list",
    "server/discover",
    "subscriptions/listen",
    "tools/call",
    "tools/list",
];

/// Whether `method` is a client-to-server request `version` defines.
///
/// A revision newer than this build answers with the `2026-07-28` set, for the
/// reason [`discover_defined_in`](crate::domain::discover_defined_in) gives:
/// SEP-2575 is not something a later draft would take back.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{ProtocolVersion, client_method_defined_in};
///
/// assert!(client_method_defined_in(ProtocolVersion::V2025_11_25, "ping"));
/// // SEP-2575 removed `ping` and replaced `logging/setLevel`.
/// assert!(!client_method_defined_in(ProtocolVersion::V2026_07_28, "ping"));
/// assert!(!client_method_defined_in(ProtocolVersion::V2026_07_28, "logging/setLevel"));
/// assert!(client_method_defined_in(ProtocolVersion::V2026_07_28, "subscriptions/listen"));
/// ```
#[must_use]
pub fn client_method_defined_in(version: ProtocolVersion, method: &str) -> bool {
    match version {
        ProtocolVersion::V2024_11_05
        | ProtocolVersion::V2025_03_26
        | ProtocolVersion::V2025_06_18 => STATEFUL.contains(&method),
        ProtocolVersion::V2025_11_25 => STATEFUL.contains(&method) || TASKS.contains(&method),
        ProtocolVersion::V2026_07_28 | ProtocolVersion::Unknown(_) => STATELESS.contains(&method),
    }
}

#[cfg(test)]
mod tests {
    use super::client_method_defined_in;
    use crate::domain::ProtocolVersion;

    #[test]
    fn the_handshake_exists_exactly_where_the_negotiation_says_it_does() {
        for version in [
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ] {
            assert_eq!(
                client_method_defined_in(version, "initialize"),
                crate::domain::handshake_defined_in(version),
                "initialize at {version}"
            );
            assert_eq!(
                client_method_defined_in(version, "server/discover"),
                crate::domain::discover_defined_in(version),
                "server/discover at {version}"
            );
        }
    }

    #[test]
    fn tasks_are_core_only_in_2025_11_25() {
        assert!(client_method_defined_in(
            ProtocolVersion::V2025_11_25,
            "tasks/get"
        ));
        assert!(!client_method_defined_in(
            ProtocolVersion::V2025_06_18,
            "tasks/get"
        ));
        assert!(!client_method_defined_in(
            ProtocolVersion::V2026_07_28,
            "tasks/get"
        ));
    }

    #[test]
    fn the_replaced_methods_are_gone_in_2026_07_28() {
        for method in [
            "ping",
            "logging/setLevel",
            "resources/subscribe",
            "resources/unsubscribe",
        ] {
            assert!(client_method_defined_in(
                ProtocolVersion::V2025_11_25,
                method
            ));
            assert!(
                !client_method_defined_in(ProtocolVersion::V2026_07_28, method),
                "{method}"
            );
        }
    }

    #[test]
    fn an_unknown_method_is_defined_nowhere() {
        assert!(!client_method_defined_in(
            ProtocolVersion::V2025_11_25,
            "unknown/method"
        ));
        assert!(!client_method_defined_in(
            ProtocolVersion::V2026_07_28,
            "unknown/method"
        ));
    }
}
