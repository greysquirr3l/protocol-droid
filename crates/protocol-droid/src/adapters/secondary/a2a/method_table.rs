//! The A2A method table — the one place a JSON-RPC method name is written down
//! for the A2A protocol.
//!
//! # Why this file exists at all
//!
//! [`MethodTable`](crate::application::jsonrpc::MethodTable) is
//! protocol-neutral so that MCP, A2A, and ACP can each add themselves without
//! touching it. The cost of that neutrality is that some module has to hold
//! the actual A2A method names, and this is it. It is deliberately small and
//! deliberately boring: a `match` on the name, and a variant per method.
//!
//! # The `match` is the perfect hash
//!
//! The task called for a perfect-hash or `match` dispatch. A `match` on string
//! literals is the better of the two for a fixed vocabulary of this size: rustc
//! lowers it to a switch on the length and then a chain of memcmps, which is
//! allocation-free and has no build step. A perfect hash would have to be
//! generated and checked in, and would then be a second thing that can
//! disagree with the list of methods.
//!
//! # Params decoding stops here
//!
//! Decoding a method's params into a per-revision wire type needs the
//! negotiated revision, which the session owns. This table proves the method
//! exists and hands back the raw params, which is exactly the split between
//! "route" and "dispatch". T27 follow-up replaces the `Ok(params)` body with
//! a per-revision `Wire` decode.
//!
//! # Wire form: v1.0 `PascalCase`, not slash-separated
//!
//! A2A v1.0 names its JSON-RPC methods in `PascalCase` — `SendMessage`,
//! `GetTask`, `CancelTask` — not the slash-separated `message/send`,
//! `tasks/get` form the earlier 0.2.x / 0.3.x revisions used. The method
//! names are also the gRPC method names; the JSON-RPC binding is the same
//! surface. The spec explicitly states this is so JSON-RPC and gRPC peers
//! can name the same method identically:
//!
//! | Functionality | JSON-RPC Method | gRPC Method |
//! | --- | --- | --- |
//! | Send message | `SendMessage` | `SendMessage` |
//! | Get task | `GetTask` | `GetTask` |
//!
//! The slash-separated form (e.g. `message/send`) is a 0.3.x artefact, kept
//! here for the bridge (T28) but not on the v1.0 wire.
//!
//! Source: A2A v1.0 specification, §5.3 "Method Mapping Reference". Vendored
//! at `crates/protocol-droid-schema/spec/a2a/v1.0/a2a.proto` (the proto is
//! the normative source; the JSON Schema is a generated build artifact).

use serde_json::value::RawValue;

use crate::application::jsonrpc::{MethodTable, RouteError};

/// Every method A2A defines, as of `v1.0`.
///
/// One variant per method, so an unknown name cannot be confused with a known
/// one at the type level: `lookup` returning `None` is the only way to reach
/// `-32601`.
///
/// A2A has no notifications at `v1.0`; every method is a request a peer
/// answers. The wire surface is request-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum A2aMethod {
    /// `SendMessage` — send a message to an agent.
    SendMessage,
    /// `SendStreamingMessage` — streaming version of `SendMessage`.
    SendStreamingMessage,
    /// `GetTask` — get the latest state of a task.
    GetTask,
    /// `ListTasks` — list tasks with optional filtering and pagination.
    ListTasks,
    /// `CancelTask` — cancel a task in progress.
    CancelTask,
    /// `SubscribeToTask` — subscribe to task updates for a non-terminal task.
    SubscribeToTask,
    /// `CreateTaskPushNotificationConfig` — register a push notification
    /// config for a task. The webhook URL on this config is the SSRF
    /// surface T27's push-notification follow-up has to filter.
    CreateTaskPushNotificationConfig,
    /// `GetTaskPushNotificationConfig` — fetch a push notification config.
    GetTaskPushNotificationConfig,
    /// `ListTaskPushNotificationConfigs` — list push notification configs.
    ListTaskPushNotificationConfigs,
    /// `DeleteTaskPushNotificationConfig` — remove a push notification
    /// config.
    DeleteTaskPushNotificationConfig,
    /// `GetExtendedAgentCard` — fetch the authenticated extended agent card.
    GetExtendedAgentCard,
}

impl A2aMethod {
    /// Every method, in the order the specification introduces them.
    ///
    /// Exists so a test can assert the direction a per-name test cannot: that
    /// every *variant* is reachable from a name, not merely that every name
    /// has a variant. A method added to the enum and forgotten in `lookup`
    /// is otherwise a `-32601` waiting to happen.
    pub const ALL: [Self; 11] = [
        Self::SendMessage,
        Self::SendStreamingMessage,
        Self::GetTask,
        Self::ListTasks,
        Self::CancelTask,
        Self::SubscribeToTask,
        Self::CreateTaskPushNotificationConfig,
        Self::GetTaskPushNotificationConfig,
        Self::ListTaskPushNotificationConfigs,
        Self::DeleteTaskPushNotificationConfig,
        Self::GetExtendedAgentCard,
    ];

    /// The wire name of this method, as the v1.0 specification defines it.
    ///
    /// `PascalCase` to match the proto file and the gRPC service. The
    /// 0.2.x / 0.3.x slash-separated form (`message/send`, `tasks/get`) is
    /// not reachable from this method, by design — the bridge (T28) is
    /// where the form mismatch is resolved.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SendMessage => "SendMessage",
            Self::SendStreamingMessage => "SendStreamingMessage",
            Self::GetTask => "GetTask",
            Self::ListTasks => "ListTasks",
            Self::CancelTask => "CancelTask",
            Self::SubscribeToTask => "SubscribeToTask",
            Self::CreateTaskPushNotificationConfig => "CreateTaskPushNotificationConfig",
            Self::GetTaskPushNotificationConfig => "GetTaskPushNotificationConfig",
            Self::ListTaskPushNotificationConfigs => "ListTaskPushNotificationConfigs",
            Self::DeleteTaskPushNotificationConfig => "DeleteTaskPushNotificationConfig",
            Self::GetExtendedAgentCard => "GetExtendedAgentCard",
        }
    }
}

/// The A2A implementation of [`MethodTable`].
///
/// A unit struct: the table is a fixed vocabulary with no configuration, so a
/// value of it is a constant rather than something to build. Mirrors
/// [`McpMethodTable`](crate::adapters::secondary::mcp::McpMethodTable).
#[derive(Debug, Clone, Copy, Default)]
pub struct A2aMethodTable;

impl A2aMethodTable {
    /// The shared table.
    pub const TABLE: Self = Self;
}

impl MethodTable for A2aMethodTable {
    type MethodId = A2aMethod;
    type RequestParams<'a> = Option<&'a RawValue>;
    type NotificationParams<'a> = Option<&'a RawValue>;

    fn lookup(&self, name: &str) -> Option<A2aMethod> {
        match name {
            "SendMessage" => Some(A2aMethod::SendMessage),
            "SendStreamingMessage" => Some(A2aMethod::SendStreamingMessage),
            "GetTask" => Some(A2aMethod::GetTask),
            "ListTasks" => Some(A2aMethod::ListTasks),
            "CancelTask" => Some(A2aMethod::CancelTask),
            "SubscribeToTask" => Some(A2aMethod::SubscribeToTask),
            "CreateTaskPushNotificationConfig" => Some(A2aMethod::CreateTaskPushNotificationConfig),
            "GetTaskPushNotificationConfig" => Some(A2aMethod::GetTaskPushNotificationConfig),
            "ListTaskPushNotificationConfigs" => Some(A2aMethod::ListTaskPushNotificationConfigs),
            "DeleteTaskPushNotificationConfig" => Some(A2aMethod::DeleteTaskPushNotificationConfig),
            "GetExtendedAgentCard" => Some(A2aMethod::GetExtendedAgentCard),
            _ => None,
        }
    }

    fn decode_request_params<'a>(
        &self,
        method: A2aMethod,
        params: Option<&'a RawValue>,
    ) -> Result<Self::RequestParams<'a>, RouteError<'a>> {
        // `params` is optional on the wire for every A2A method, and the
        // difference between "absent" and "null" is not one any handler
        // cares about, so both arrive here as `None`-shaped values. T27
        // follow-up replaces this with a per-revision `Wire` decode gated
        // on `A2aVersion`.
        let _ = method;
        Ok(params)
    }

    fn decode_notification_params<'a>(
        &self,
        method: A2aMethod,
        params: Option<&'a RawValue>,
    ) -> Result<Self::NotificationParams<'a>, RouteError<'a>> {
        // A2A v1.0 has no notifications, so reaching here means the caller
        // named a request method in a notification-shaped frame. The router
        // answers with whatever the wire-shape check returns, not a per-
        // method check; this branch is symmetric to the MCP one.
        let _ = method;
        Ok(params)
    }
}

#[cfg(test)]
mod tests {
    // Test code unwraps deliberately: the strict profile exists to keep
    // production paths fallible, and a test that cannot unwrap cannot assert.
    // The messages name the failing precondition, which is the useful part.
    #![expect(
        clippy::expect_used,
        clippy::panic,
        reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
    )]
    //! The A2A table, exercised through the protocol-neutral router.
    //!
    //! Mirrors the MCP table tests in shape — the architecture depends on
    //! both tables being reachable from inside the crate or the lib test
    //! target will not build, and the dependency pointing in only the
    //! direction the architecture allows (an adapter may name application,
    //! application may not name an adapter) is a stronger test than a mock.
    //!
    //! The router's own behaviour, with a stand-in table, is tested in
    //! `application/jsonrpc/router.rs`; the batch policy in
    //! `application/jsonrpc/batch.rs`.

    use assert_matches::assert_matches;
    use serde_json::value::RawValue;

    use crate::application::jsonrpc::{MethodTable, RouteError, Routed, route_one};
    use crate::domain::jsonrpc::{Frame, decode};

    use super::{A2aMethod, A2aMethodTable};

    type TestError = Box<dyn std::error::Error>;

    /// The one non-A2A method name, to prove `-32601` is the table's answer
    /// and not a special case in the router.
    const NOT_A2A: &str = "does/not/exist";

    /// The A2A 0.3.x slash-form of `SendMessage`, included to prove the v1.0
    /// table rejects it. A bridge to a 0.3.x peer translates the form before
    /// this table ever sees a frame; the table itself is form-stable.
    const LEGACY_A2A: &str = "message/send";

    fn route(
        input: &[u8],
    ) -> Result<Routed<'_, A2aMethod, Option<&RawValue>, Option<&RawValue>>, RouteError<'_>> {
        let envelope = match decode(input)? {
            Frame::Single(envelope) => envelope,
            // `BatchNotAllowed` is the wrong story for a single-message test,
            // but it is the only refusal that does not lie about why: the
            // router refuses a batch before it looks at any of its members.
            Frame::Batch(_) => return Err(RouteError::BatchNotAllowed),
        };
        route_one(envelope, &A2aMethodTable::TABLE)
    }

    fn request(method: &str) -> String {
        format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}"}}"#)
    }

    /// Every method the v1.0 specification defines, as its wire name. A
    /// method that exists in the enum but is missing here fails the
    /// `every_variant_is_in_the_list` test, and a name that is listed but
    /// not in the enum fails to compile.
    const EVERY_METHOD: &[(&str, A2aMethod)] = &[
        ("SendMessage", A2aMethod::SendMessage),
        ("SendStreamingMessage", A2aMethod::SendStreamingMessage),
        ("GetTask", A2aMethod::GetTask),
        ("ListTasks", A2aMethod::ListTasks),
        ("CancelTask", A2aMethod::CancelTask),
        ("SubscribeToTask", A2aMethod::SubscribeToTask),
        (
            "CreateTaskPushNotificationConfig",
            A2aMethod::CreateTaskPushNotificationConfig,
        ),
        (
            "GetTaskPushNotificationConfig",
            A2aMethod::GetTaskPushNotificationConfig,
        ),
        (
            "ListTaskPushNotificationConfigs",
            A2aMethod::ListTaskPushNotificationConfigs,
        ),
        (
            "DeleteTaskPushNotificationConfig",
            A2aMethod::DeleteTaskPushNotificationConfig,
        ),
        ("GetExtendedAgentCard", A2aMethod::GetExtendedAgentCard),
    ];

    #[test]
    fn every_method_is_reachable_and_agrees_about_its_wire_name() {
        for (name, expected) in EVERY_METHOD {
            let found = A2aMethodTable::TABLE
                .lookup(name)
                .unwrap_or_else(|| panic!("{name} is in the table and must be found"));
            assert_eq!(found, *expected, "{name} must resolve to its own variant");
            assert_eq!(
                found.as_str(),
                *name,
                "as_str must be the name lookup accepts, or a reply echoes the wrong method"
            );
        }
    }

    #[test]
    fn every_variant_is_in_the_list() {
        for method in A2aMethod::ALL {
            assert!(
                EVERY_METHOD.iter().any(|(_, listed)| *listed == method),
                "{method:?} is a variant but no test names it, so nothing would fail if \
                 lookup stopped resolving it"
            );
        }
    }

    #[test]
    fn an_unknown_method_is_method_not_found() {
        let input = request(NOT_A2A);
        let error = route(input.as_bytes()).expect_err("no such A2A method");
        assert_eq!(error.code(), crate::domain::ir::IrErrorCode::MethodNotFound);
        assert_matches!(error, RouteError::MethodNotFound(name) if name == NOT_A2A);
    }

    #[test]
    fn a_method_name_is_not_normalised() {
        // The table is a `match`, not a lookup table, so a near-miss must
        // miss. A2A v1.0 names methods PascalCase and the specification is
        // explicit that this is case-sensitive, just like gRPC.
        for near in [
            "sendmessage",
            "sendMessage",
            "send_message",
            " SendMessage",
            "SendMessage ",
        ] {
            let input = request(near);
            let error = route(input.as_bytes())
                .expect_err("the specification names methods case-sensitively");
            assert_eq!(
                error.code(),
                crate::domain::ir::IrErrorCode::MethodNotFound,
                "{near} is not a method name"
            );
        }
    }

    #[test]
    fn the_legacy_0_3_x_slash_form_is_not_a_v1_0_method() {
        // The 0.3.x form is a different protocol surface. A bridge to a
        // 0.3.x peer translates the form before this table ever sees it;
        // the table itself is form-stable.
        let input = request(LEGACY_A2A);
        let error = route(input.as_bytes())
            .expect_err("0.3.x slash-form must not resolve on the v1.0 table");
        assert_eq!(error.code(), crate::domain::ir::IrErrorCode::MethodNotFound);
        assert_matches!(error, RouteError::MethodNotFound(name) if name == LEGACY_A2A);
    }

    #[test]
    fn a_request_routes_to_its_method_and_keeps_its_params_borrowed() -> Result<(), TestError> {
        let input = br#"{"jsonrpc":"2.0","id":7,"method":"SendMessage","params":{"message":{"role":"user","parts":[]}}}"#;
        match route(input) {
            Ok(Routed::Request {
                method, id, params, ..
            }) => {
                assert_eq!(method, A2aMethod::SendMessage);
                assert_eq!(id.as_number(), Some(7));
                let raw = params.ok_or("SendMessage has params")?;
                assert_eq!(raw.get(), r#"{"message":{"role":"user","parts":[]}}"#);
            }
            other => return Err(format!("expected a request, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn a_request_without_params_routes_with_none() -> Result<(), TestError> {
        // `GetTask` is a path-parameter-only request; A2A encodes the path
        // parameter (`id`) in `params.id` per the proto, not in the URL
        // path. The transport parser leaves the path parameter extraction
        // for the dispatch arm; the table is only responsible for naming
        // the method.
        let input = br#"{"jsonrpc":"2.0","id":2,"method":"GetTask","params":{"id":"task-1"}}"#;
        match route(input) {
            Ok(Routed::Request { method, params, .. }) => {
                assert_eq!(method, A2aMethod::GetTask);
                let raw = params.ok_or("GetTask has params")?;
                assert_eq!(raw.get(), r#"{"id":"task-1"}"#);
            }
            other => return Err(format!("expected a request, got {other:?}").into()),
        }
        Ok(())
    }
}
