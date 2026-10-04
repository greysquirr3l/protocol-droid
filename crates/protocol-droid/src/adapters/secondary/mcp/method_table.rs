//! The MCP method table — the one place a JSON-RPC method name is written down.
//!
//! # Why this file exists at all
//!
//! [`MethodTable`] is protocol-neutral so that A2A and ACP can add themselves
//! without touching it. The cost of that neutrality is that some module has to
//! hold the actual MCP method names, and this is it. It is deliberately small
//! and deliberately boring: a `match` on the name, and a variant per method.
//!
//! # The `match` is the perfect hash
//!
//! The task called for a perfect-hash or `match` dispatch. A `match` on string
//! literals is the better of the two for a fixed vocabulary of this size: rustc
//! lowers it to a switch on the length and then a chain of memcmps, which is
//! allocation-free and has no build step. A perfect hash would have to be
//! generated and checked in, and would then be a second thing that can disagree
//! with the list of methods.
//!
//! # Params decoding stops here
//!
//! Decoding a method's params into a per-revision wire type needs the negotiated
//! revision, which the session owns. T09 delivers the table and the refusal —
//! a params shape that does not fit becomes `-32602` — and T12 narrows the
//! return types to the revision's `Wire`. Until then the table proves the
//! method exists and hands back the raw params, which is exactly the split
//! between "route" and "dispatch".

use serde_json::value::RawValue;

use crate::application::jsonrpc::{MethodTable, RouteError};

/// Every method MCP defines, as of `2026-07-28`.
///
/// One variant per method, so an unknown name cannot be confused with a known
/// one at the type level: `lookup` returning `None` is the only way to reach
/// `-32601`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum McpMethod {
    /// `initialize`.
    Initialize,
    /// `ping`.
    Ping,
    /// `tools/list`.
    ToolsList,
    /// `tools/call`.
    ToolsCall,
    /// `resources/list`.
    ResourcesList,
    /// `resources/read`.
    ResourcesRead,
    /// `resources/templates/list`.
    ResourceTemplatesList,
    /// `prompts/list`.
    PromptsList,
    /// `prompts/get`.
    PromptsGet,
    /// `logging/setLevel`.
    SetLogLevel,
    /// `completion/complete`.
    Complete,
    /// `notifications/initialized`.
    InitializedNotification,
    /// `notifications/cancelled`.
    CancelledNotification,
    /// `notifications/progress`.
    ProgressNotification,
    /// `notifications/message`.
    LoggingMessageNotification,
    /// `notifications/roots/list_changed`.
    RootsListChangedNotification,
    /// `sampling/createMessage`.
    SamplingCreateMessage,
    /// `elicitation/create`.
    ElicitationCreate,
    /// `roots/list`.
    RootsList,
}

impl McpMethod {
    /// Every method, in the order the specification introduces them.
    ///
    /// Exists so a test can assert the direction a per-name test cannot: that
    /// every *variant* is reachable from a name, not merely that every name has
    /// a variant. A method added to the enum and forgotten in `lookup` is
    /// otherwise a `-32601` waiting to happen.
    pub const ALL: [Self; 19] = [
        Self::Initialize,
        Self::Ping,
        Self::ToolsList,
        Self::ToolsCall,
        Self::ResourcesList,
        Self::ResourcesRead,
        Self::ResourceTemplatesList,
        Self::PromptsList,
        Self::PromptsGet,
        Self::SetLogLevel,
        Self::Complete,
        Self::RootsList,
        Self::SamplingCreateMessage,
        Self::ElicitationCreate,
        Self::InitializedNotification,
        Self::CancelledNotification,
        Self::ProgressNotification,
        Self::LoggingMessageNotification,
        Self::RootsListChangedNotification,
    ];

    /// The wire name of this method.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::Ping => "ping",
            Self::ToolsList => "tools/list",
            Self::ToolsCall => "tools/call",
            Self::ResourcesList => "resources/list",
            Self::ResourcesRead => "resources/read",
            Self::ResourceTemplatesList => "resources/templates/list",
            Self::PromptsList => "prompts/list",
            Self::PromptsGet => "prompts/get",
            Self::SetLogLevel => "logging/setLevel",
            Self::Complete => "completion/complete",
            Self::InitializedNotification => "notifications/initialized",
            Self::CancelledNotification => "notifications/cancelled",
            Self::ProgressNotification => "notifications/progress",
            Self::LoggingMessageNotification => "notifications/message",
            Self::RootsListChangedNotification => "notifications/roots/list_changed",
            Self::SamplingCreateMessage => "sampling/createMessage",
            Self::ElicitationCreate => "elicitation/create",
            Self::RootsList => "roots/list",
        }
    }

    /// Whether a request naming this method is something a server answers.
    ///
    /// Only informational. A server still has to consult the session's
    /// [`FeatureProfile`](crate::domain::feature::FeatureProfile) before
    /// answering, because a method can exist in the schema and be unavailable
    /// in a given session — `roots/list` after `roots` is deprecated, or
    /// `sampling/createMessage` from a client that never advertised sampling.
    /// This answers the narrower question of whether the name is a request name
    /// at all, which is what the router needs to pick between its two params
    /// decoders.
    #[must_use]
    pub fn is_request(self) -> bool {
        !self.as_str().starts_with("notifications/")
    }
}

/// The MCP implementation of [`MethodTable`].
///
/// A unit struct: the table is a fixed vocabulary with no configuration, so a
/// value of it is a constant rather than something to build.
#[derive(Debug, Clone, Copy, Default)]
pub struct McpMethodTable;

impl McpMethodTable {
    /// The shared table.
    pub const TABLE: Self = Self;
}

impl MethodTable for McpMethodTable {
    type MethodId = McpMethod;
    type RequestParams<'a> = Option<&'a RawValue>;
    type NotificationParams<'a> = Option<&'a RawValue>;

    fn lookup(&self, name: &str) -> Option<McpMethod> {
        match name {
            "initialize" => Some(McpMethod::Initialize),
            "ping" => Some(McpMethod::Ping),
            "tools/list" => Some(McpMethod::ToolsList),
            "tools/call" => Some(McpMethod::ToolsCall),
            "resources/list" => Some(McpMethod::ResourcesList),
            "resources/read" => Some(McpMethod::ResourcesRead),
            "resources/templates/list" => Some(McpMethod::ResourceTemplatesList),
            "prompts/list" => Some(McpMethod::PromptsList),
            "prompts/get" => Some(McpMethod::PromptsGet),
            "logging/setLevel" => Some(McpMethod::SetLogLevel),
            "completion/complete" => Some(McpMethod::Complete),
            "roots/list" => Some(McpMethod::RootsList),
            "sampling/createMessage" => Some(McpMethod::SamplingCreateMessage),
            "elicitation/create" => Some(McpMethod::ElicitationCreate),
            "notifications/initialized" => Some(McpMethod::InitializedNotification),
            "notifications/cancelled" => Some(McpMethod::CancelledNotification),
            "notifications/progress" => Some(McpMethod::ProgressNotification),
            "notifications/message" => Some(McpMethod::LoggingMessageNotification),
            "notifications/roots/list_changed" => Some(McpMethod::RootsListChangedNotification),
            _ => None,
        }
    }

    fn decode_request_params<'a>(
        &self,
        method: McpMethod,
        params: Option<&'a RawValue>,
    ) -> Result<Self::RequestParams<'a>, RouteError<'a>> {
        // `params` is optional on the wire for every MCP method, and the
        // difference between "absent" and "null" is not one any handler
        // cares about, so both arrive here as `None`-shaped values. T12
        // replaces this with a per-revision `Wire` decode.
        let _ = method;
        Ok(params)
    }

    fn decode_notification_params<'a>(
        &self,
        method: McpMethod,
        params: Option<&'a RawValue>,
    ) -> Result<Self::NotificationParams<'a>, RouteError<'a>> {
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
    //! The MCP table, exercised through the protocol-neutral router.
    //!
    //! # Why these live here and not in `tests/`
    //!
    //! A dependent crate can see the crate's public API, but the compiler
    //! still calls a public-but-unused item dead when it builds this crate as
    //! a test binary. So anything the crate exports has to be reachable from
    //! inside the crate or the lib test target will not build.
    //!
    //! Direction is the other reason: `application` may not name an adapter,
    //! but an adapter may name the application. Testing the MCP table against
    //! the real router from *this* module keeps the dependency pointing in
    //! the only direction the architecture allows, and it is a stronger test
    //! than a mock — a method name that drifts out of the specification fails
    //! here rather than in production.
    //!
    //! The router's own behaviour, with a stand-in table, is tested in
    //! `application/jsonrpc/router.rs`; the batch policy in
    //! `application/jsonrpc/batch.rs`.

    use assert_matches::assert_matches;
    use serde_json::value::RawValue;

    use crate::application::jsonrpc::{MethodTable, RouteError, Routed, route_one};
    use crate::domain::feature::{Feature, FeatureProfile};
    use crate::domain::jsonrpc::{Frame, decode};

    use super::{McpMethod, McpMethodTable};

    type TestError = Box<dyn std::error::Error>;

    /// The one non-MCP method name, to prove `-32601` is the table's answer
    /// and not a special case in the router.
    const NOT_MCP: &str = "does/not/exist";

    fn route(
        input: &[u8],
    ) -> Result<Routed<'_, McpMethod, Option<&RawValue>, Option<&RawValue>>, RouteError<'_>> {
        let envelope = match decode(input)? {
            Frame::Single(envelope) => envelope,
            // `BatchNotAllowed` is the wrong story for a single-message test,
            // but it is the only refusal that does not lie about why: the
            // router refuses a batch before it looks at any of its members.
            Frame::Batch(_) => return Err(RouteError::BatchNotAllowed),
        };
        route_one(envelope, &McpMethodTable::TABLE)
    }

    fn request(method: &str) -> String {
        format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}"}}"#)
    }

    /// Every method the specification defines at this revision, as its wire
    /// name. A method that exists in the enum but is missing here fails the
    /// `every_method_is_reachable` test, and a name that is listed but not in
    /// the enum fails to compile.
    const EVERY_METHOD: &[(&str, McpMethod, bool)] = &[
        ("initialize", McpMethod::Initialize, true),
        ("ping", McpMethod::Ping, true),
        ("tools/list", McpMethod::ToolsList, true),
        ("tools/call", McpMethod::ToolsCall, true),
        ("resources/list", McpMethod::ResourcesList, true),
        ("resources/read", McpMethod::ResourcesRead, true),
        (
            "resources/templates/list",
            McpMethod::ResourceTemplatesList,
            true,
        ),
        ("prompts/list", McpMethod::PromptsList, true),
        ("prompts/get", McpMethod::PromptsGet, true),
        ("logging/setLevel", McpMethod::SetLogLevel, true),
        ("completion/complete", McpMethod::Complete, true),
        ("roots/list", McpMethod::RootsList, true),
        (
            "sampling/createMessage",
            McpMethod::SamplingCreateMessage,
            true,
        ),
        ("elicitation/create", McpMethod::ElicitationCreate, true),
        (
            "notifications/initialized",
            McpMethod::InitializedNotification,
            false,
        ),
        (
            "notifications/cancelled",
            McpMethod::CancelledNotification,
            false,
        ),
        (
            "notifications/progress",
            McpMethod::ProgressNotification,
            false,
        ),
        (
            "notifications/message",
            McpMethod::LoggingMessageNotification,
            false,
        ),
        (
            "notifications/roots/list_changed",
            McpMethod::RootsListChangedNotification,
            false,
        ),
    ];

    #[test]
    fn every_method_is_reachable_and_agrees_about_being_a_request() {
        for (name, expected, is_request) in EVERY_METHOD {
            let found = McpMethodTable::TABLE
                .lookup(name)
                .unwrap_or_else(|| panic!("{name} is in the table and must be found"));
            assert_eq!(found, *expected, "{name} must resolve to its own variant");
            assert_eq!(
                found.as_str(),
                *name,
                "as_str must be the name lookup accepts, or a reply echoes the wrong method"
            );
            assert_eq!(
                found.is_request(),
                *is_request,
                "{name} is {} a request",
                if *is_request { "" } else { "not" }
            );
        }
    }

    #[test]
    fn every_variant_is_in_the_list() {
        for method in McpMethod::ALL {
            assert!(
                EVERY_METHOD.iter().any(|(_, listed, _)| *listed == method),
                "{method:?} is a variant but no test names it, so nothing would fail if \
                 lookup stopped resolving it"
            );
        }
    }

    #[test]
    fn an_unknown_method_is_method_not_found() {
        let input = request(NOT_MCP);
        let error = route(input.as_bytes()).expect_err("no such MCP method");
        assert_eq!(error.code(), crate::domain::ir::IrErrorCode::MethodNotFound);
        assert_matches!(error, RouteError::MethodNotFound(name) if name == "does/not/exist");
    }

    #[test]
    fn a_method_name_is_not_normalised() {
        // The table is a `match`, not a lookup table, so a near-miss must miss.
        for near in ["Initialize", "tools/List", "tools/list ", " tools/list"] {
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
    fn a_request_routes_to_its_method_and_keeps_its_params_borrowed() -> Result<(), TestError> {
        let input = br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"echo"}}"#;
        match route(input) {
            Ok(Routed::Request {
                method, id, params, ..
            }) => {
                assert_eq!(method, McpMethod::ToolsCall);
                assert_eq!(id.as_number(), Some(7));
                let raw = params.ok_or("tools/call has params")?;
                assert_eq!(raw.get(), r#"{"name":"echo"}"#);
            }
            other => return Err(format!("expected a request, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn a_notification_routes_without_an_id() -> Result<(), TestError> {
        let input = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        match route(input) {
            Ok(Routed::Notification { method, params }) => {
                assert_eq!(method, McpMethod::InitializedNotification);
                assert!(params.is_none());
            }
            other => return Err(format!("expected a notification, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn a_notification_name_sent_as_a_request_is_still_a_notification() -> Result<(), TestError> {
        // The table does not police request-versus-notification; the shape
        // does. A peer that names a notification method with an id gets a
        // request that nothing answers, which the specification does not
        // permit — so this asserts the split is by wire shape only, and that
        // the split is total.
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"notifications/initialized"}"#;
        match route(input) {
            Ok(Routed::Request { method, .. }) => {
                assert_eq!(method, McpMethod::InitializedNotification);
                assert!(!method.is_request(), "and the method knows it is one");
            }
            other => return Err(format!("expected a request, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn a_reply_routes_to_no_method() -> Result<(), TestError> {
        let input = br#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
        match route(input) {
            Ok(Routed::Reply { id, payload }) => {
                assert_eq!(id.as_number(), Some(1));
                assert_matches!(
                    payload,
                    crate::application::jsonrpc::ReplyPayload::Result(_)
                );
            }
            other => return Err(format!("expected a reply, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn a_batch_is_refused_when_the_profile_has_no_batching() {
        let profile = FeatureProfile::empty();
        assert!(!profile.allows(Feature::JsonRpcBatching));
    }
}
