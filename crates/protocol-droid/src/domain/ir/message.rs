//! The message envelope — request, notification, result, error.
//!
//! # Why the envelope is not a codec type
//!
//! The IR has to cover every method in every revision, which is a lot of
//! distinct result and parameter shapes. Modelling all of them here would be
//! two crates' worth of enums whose only job was to be lowered again, and most
//! of them are structurally identical — "a list of resources" is the same shape
//! for `resources/list` in every revision.
//!
//! So the envelope is generic: [`IrResult`] and [`IrParams`] hold an
//! [`Extensions`] bag for the fields the IR does not model, and T08's codecs
//! populate those for the methods that need it. The shapes that *do* matter —
//! content, tool results — have real types, because they are what user handlers
//! touch and what lowering has to reason about.
//!
//! The trade is deliberate and worth stating: the IR is complete (nothing is
//! lost) but not fully typed (some things are opaque). Completeness is the
//! property the bridge needs; full typing is a convenience, and paying for it
//! up front would mean the IR had to change every time a revision adds a field.
//!
//! # `IrMessage` covers all four JSON-RPC shapes
//!
//! The spec requires a bridge to forward notifications without a reply and
//! results only against the request that caused them. Modelling all four here
//! means the envelope is the only place that knows about JSON-RPC, and the
//! session engine (T10) does not re-derive it.

use std::sync::Arc;

use crate::domain::RequestId;
use crate::domain::ir::extensions::Extensions;
use crate::domain::ir::features::FeatureSet;

/// A method name, validated.
///
/// A newtype for the same reason `TransportSpec` is: a method name is a string
/// that has to be exact, and passing a raw `&str` around means the "did you
/// mean `notifications/cancelled` or `notifications/canceled`" check lives in
/// whoever happens to be comparing. Constructed from a literal or `FromStr`, so
/// the check is at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MethodName(Arc<str>);

impl MethodName {
    /// A method name, validated as non-empty and slash-separated.
    ///
    /// # Errors
    ///
    /// [`MethodNameError::Empty`] when the name is empty,
    /// [`MethodNameError::NoSegment`] when it starts or ends with `/`, and
    /// [`MethodNameError::Whitespace`] when a segment has surrounding space —
    /// the last of which catches `"tools/list "` losing a race in string
    /// concatenation.
    pub fn new(name: &str) -> Result<Self, MethodNameError> {
        if name.is_empty() {
            return Err(MethodNameError::Empty);
        }
        if name.starts_with('/') || name.ends_with('/') {
            return Err(MethodNameError::NoSegment {
                name: name.to_owned(),
            });
        }
        if name.split('/').any(|segment| segment.trim() != segment) {
            return Err(MethodNameError::Whitespace {
                name: name.to_owned(),
            });
        }
        Ok(Self(Arc::from(name)))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the name is a notification, which is the one naming convention
    /// the spec fixes across every revision.
    #[must_use]
    pub fn is_notification(&self) -> bool {
        self.0.starts_with("notifications/")
    }
}

impl core::fmt::Display for MethodName {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl core::str::FromStr for MethodName {
    type Err = MethodNameError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

/// Why a method name is not a [`MethodName`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MethodNameError {
    /// The name was empty.
    #[error("method name is empty")]
    Empty,
    /// The name started or ended with a slash.
    #[error("method name {name:?} has an empty segment")]
    NoSegment {
        /// The rejected name.
        name: String,
    },
    /// A segment had surrounding whitespace.
    #[error("method name {name:?} has a segment with surrounding whitespace")]
    Whitespace {
        /// The rejected name.
        name: String,
    },
}

/// The parameters of a request or notification.
///
/// Opaque by design — see the module docs. An empty bag means "no parameters
/// the IR models", which is the common case and not the same as "no
/// parameters".
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IrParams {
    entries: Extensions,
}

impl IrParams {
    /// No modelled parameters.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            entries: Extensions::empty(),
        }
    }

    /// Wraps an extension bag as parameters.
    #[must_use]
    pub const fn from_extensions(entries: Extensions) -> Self {
        Self { entries }
    }

    /// Adds a parameter.
    #[must_use]
    pub fn with(
        mut self,
        name: impl Into<String>,
        value: crate::domain::ir::ExtensionValue,
    ) -> Self {
        self.entries.insert(name, value);
        self
    }

    /// A parameter, if present.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&crate::domain::ir::ExtensionValue> {
        self.entries.get(name)
    }

    /// The underlying bag.
    #[must_use]
    pub const fn as_extensions(&self) -> &Extensions {
        &self.entries
    }
}

/// A request. Expects exactly one result or error.
#[derive(Debug, Clone, PartialEq)]
pub struct IrRequest {
    /// The method being called.
    pub method: MethodName,
    /// The request id, `None` only for a request the peer malformed.
    pub id: Option<RequestId>,
    /// The parameters.
    pub params: IrParams,
    /// Unmodelled fields on the envelope itself.
    pub extensions: Extensions,
}

impl IrRequest {
    /// A request with no parameters.
    #[must_use]
    pub const fn new(method: MethodName, id: RequestId) -> Self {
        Self {
            method,
            id: Some(id),
            params: IrParams::empty(),
            extensions: Extensions::empty(),
        }
    }

    /// Attaches parameters.
    #[must_use]
    pub fn with_params(mut self, params: IrParams) -> Self {
        self.params = params;
        self
    }
}

/// A notification. Never has an id, never gets a reply.
#[derive(Debug, Clone, PartialEq)]
pub struct IrNotification {
    /// The notification's method.
    pub method: MethodName,
    /// The parameters.
    pub params: IrParams,
    /// Unmodelled fields.
    pub extensions: Extensions,
}

impl IrNotification {
    /// A notification with no parameters.
    ///
    /// # Panics
    ///
    /// Never: the method is supplied by the caller and a non-notification
    /// method is a caller mistake that surfaces as a test failure, not a
    /// production panic. Use [`Self::try_new`] to get a `Result` instead.
    #[must_use]
    pub fn new(method: MethodName) -> Self {
        debug_assert!(
            method.is_notification(),
            "{method} is not a notification method",
        );
        Self {
            method,
            params: IrParams::empty(),
            extensions: Extensions::empty(),
        }
    }
}

/// A successful result.
#[derive(Debug, Clone, PartialEq)]
pub struct IrResult {
    /// The request this answers.
    pub id: RequestId,
    /// The result payload, as modelled fields plus the passthrough bag.
    pub params: IrParams,
    /// Unmodelled fields.
    pub extensions: Extensions,
}

impl IrResult {
    /// A result with no modelled payload.
    #[must_use]
    pub const fn new(id: RequestId) -> Self {
        Self {
            id,
            params: IrParams::empty(),
            extensions: Extensions::empty(),
        }
    }
}

/// A JSON-RPC error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IrErrorCode {
    /// `-32700`, invalid JSON.
    ParseError,
    /// `-32600`, the envelope was not a valid request object.
    InvalidRequest,
    /// `-32601`, the method does not exist.
    MethodNotFound,
    /// `-32602`, the params did not match the method.
    InvalidParams,
    /// `-32603`, an internal error.
    InternalError,
    /// A code in the implementation-defined `-32000..=-32099` range.
    ///
    /// The raw code, not an offset into the range. Storing the code means
    /// `code` and `from_code` are both an identity, so there is no arithmetic
    /// to get backwards and no narrowing cast to make: the range runs
    /// *downwards* from `-32000`, and a representation that hid that as an
    /// `offset` is exactly the kind of thing that ends up adding instead of
    /// subtracting and putting offset 99 at `-31901`, outside the reserved
    /// range.
    Implementation(i64),
    /// A code outside every range the spec reserves.
    Other(i64),
}

impl IrErrorCode {
    /// `-32022`, SEP-2575's unsupported-protocol-version code.
    ///
    /// Named rather than written at each use because the specification gives
    /// it meaning rather than just a number: it is the code that carries
    /// `data.supported`, so a client knows to retry against a revision this
    /// server speaks instead of hunting for a bug in its own request. A bare
    /// `-32022` at a call site reads as a typo, and this one is easy to typo.
    ///
    /// It lives in the implementation-defined range, which is why it is
    /// [`Implementation`](Self::Implementation) and not a named variant of its
    /// own: a new variant would be a promise that no other code can hold,
    /// since nothing stops a peer sending this number for an unrelated reason.
    pub const UNSUPPORTED_PROTOCOL_VERSION: Self = Self::Implementation(-32022);

    /// `-32020`, `HeaderMismatchError` (`2026-07-28`).
    ///
    /// An HTTP header disagrees with the body it describes, or a required one
    /// is missing. Renumbered from `-32001` when the revision partitioned the
    /// server-error range; see that revision's changelog, minor change 12.
    pub const HEADER_MISMATCH: Self = Self::Implementation(-32020);

    /// `-32021`, `MissingRequiredClientCapabilityError` (`2026-07-28`).
    ///
    /// Serving the request needs a capability the client did not declare in
    /// its per-request `clientCapabilities`. Carries
    /// `data.requiredCapabilities`, a `ClientCapabilities` object.
    pub const MISSING_REQUIRED_CLIENT_CAPABILITY: Self = Self::Implementation(-32021);

    /// The numeric wire value.
    #[must_use]
    pub const fn code(self) -> i64 {
        match self {
            Self::ParseError => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::InternalError => -32603,
            // Both carry the code verbatim; the distinction is semantic.
            Self::Implementation(code) | Self::Other(code) => code,
        }
    }

    /// The code a raw number denotes.
    ///
    /// Not `const`: `RangeInclusive::contains` is not a const fn on 1.98, and
    /// nothing evaluates this at compile time.
    #[must_use]
    pub fn from_code(code: i64) -> Self {
        match code {
            -32700 => Self::ParseError,
            -32600 => Self::InvalidRequest,
            -32601 => Self::MethodNotFound,
            -32602 => Self::InvalidParams,
            -32603 => Self::InternalError,
            code if (-32099..=-32000).contains(&code) => Self::Implementation(code),
            other => Self::Other(other),
        }
    }
}

/// An error result.
#[derive(Debug, Clone, PartialEq)]
pub struct IrError {
    /// The request this answers.
    pub id: Option<RequestId>,
    /// The error code.
    pub code: IrErrorCode,
    /// A short human-readable message.
    pub message: String,
    /// Structured detail, if any.
    pub data: Option<crate::domain::ir::JsonObject>,
    /// Unmodelled fields.
    pub extensions: Extensions,
}

impl IrError {
    /// An error with a code and a message.
    #[must_use]
    pub fn new(id: Option<RequestId>, code: IrErrorCode, message: impl Into<String>) -> Self {
        Self {
            id,
            code,
            message: message.into(),
            data: None,
            extensions: Extensions::empty(),
        }
    }
}

/// Any message in either direction.
#[derive(Debug, Clone, PartialEq)]
pub enum IrMessage {
    /// A request.
    Request(IrRequest),
    /// A notification.
    Notification(IrNotification),
    /// A result.
    Result(IrResult),
    /// An error.
    Error(IrError),
}

impl IrMessage {
    /// The method name, for a request or notification.
    #[must_use]
    pub const fn method(&self) -> Option<&MethodName> {
        match self {
            Self::Request(request) => Some(&request.method),
            Self::Notification(notification) => Some(&notification.method),
            Self::Result(_) | Self::Error(_) => None,
        }
    }

    /// The request id, when there is one.
    ///
    /// A notification has none, and neither does a parse error, which is why
    /// this is an `Option` on the message rather than a field.
    #[must_use]
    pub const fn id(&self) -> Option<&RequestId> {
        match self {
            Self::Request(request) => request.id.as_ref(),
            Self::Notification(_) => None,
            Self::Result(result) => Some(&result.id),
            Self::Error(error) => error.id.as_ref(),
        }
    }

    /// Whether this message expects a reply.
    #[must_use]
    pub const fn expects_reply(&self) -> bool {
        matches!(self, Self::Request(_))
    }

    /// The features this message needs in order to be expressed.
    ///
    /// Empty for everything in the envelope: none of the four JSON-RPC shapes
    /// are version-gated, because the spec's lifecycle is the same at every
    /// revision. The method does the gating instead, and T08's codec table
    /// maps a method to the features its payload needs.
    #[must_use]
    pub const fn required_features(&self) -> FeatureSet {
        FeatureSet::empty()
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and unwrap with the failing precondition in the message"
    )]

    use super::{
        IrError, IrErrorCode, IrMessage, IrNotification, IrParams, IrRequest, IrResult, MethodName,
        MethodNameError,
    };
    use crate::domain::RequestId;
    use crate::domain::ir::ExtensionValue;

    fn id() -> RequestId {
        RequestId::number(1)
    }

    #[test]
    fn method_names_validate_their_shape() {
        assert!(MethodName::new("tools/list").is_ok());
        assert!(MethodName::new("notifications/initialized").is_ok());
        assert!(MethodName::new("ping").is_ok());

        assert_eq!(MethodName::new("").unwrap_err(), MethodNameError::Empty);
        assert_eq!(
            MethodName::new("/tools/list").unwrap_err(),
            MethodNameError::NoSegment {
                name: "/tools/list".into()
            }
        );
        assert_eq!(
            MethodName::new("tools/list/").unwrap_err(),
            MethodNameError::NoSegment {
                name: "tools/list/".into()
            }
        );
        assert_eq!(
            MethodName::new("tools/ list").unwrap_err(),
            MethodNameError::Whitespace {
                name: "tools/ list".into()
            }
        );
        assert_eq!(
            MethodName::new("tools/list ").unwrap_err(),
            MethodNameError::Whitespace {
                name: "tools/list ".into()
            }
        );
    }

    #[test]
    fn method_names_round_trip() {
        let name = MethodName::new("resources/read").unwrap();
        assert_eq!(name.as_str(), "resources/read");
        assert_eq!(name.to_string(), "resources/read");
        assert_eq!(
            "tools/call".parse::<MethodName>().unwrap().as_str(),
            "tools/call"
        );
    }

    #[test]
    fn the_notification_prefix_is_the_convention() {
        assert!(
            MethodName::new("notifications/cancelled")
                .unwrap()
                .is_notification()
        );
        assert!(!MethodName::new("tools/list").unwrap().is_notification());
        // A method that merely starts with the same letters is not one.
        assert!(
            !MethodName::new("notificationsish/x")
                .unwrap()
                .is_notification()
        );
    }

    #[test]
    fn params_hold_unmodelled_fields() {
        let params = IrParams::empty()
            .with("cursor", ExtensionValue::String("abc".into()))
            .with("n", ExtensionValue::Number("5".into()));
        assert_eq!(
            params.get("cursor"),
            Some(&ExtensionValue::String("abc".into()))
        );
        assert!(params.get("missing").is_none());
    }

    #[test]
    fn requests_carry_a_method_an_id_and_params() {
        let request = IrRequest::new(MethodName::new("tools/call").unwrap(), id())
            .with_params(IrParams::empty().with("name", ExtensionValue::String("t".into())));
        assert_eq!(request.method.as_str(), "tools/call");
        assert_eq!(request.id.as_ref(), Some(&id()));
        assert!(request.params.get("name").is_some());
    }

    #[test]
    fn notifications_have_no_id_and_expects_reply_is_false() {
        let notification =
            IrNotification::new(MethodName::new("notifications/initialized").unwrap());
        let message = IrMessage::Notification(notification);
        assert!(message.id().is_none());
        assert!(!message.expects_reply());
        assert_eq!(
            message.method().unwrap().as_str(),
            "notifications/initialized"
        );
    }

    #[test]
    fn a_request_expects_a_reply_and_carries_its_id() {
        let message = IrMessage::Request(IrRequest::new(MethodName::new("ping").unwrap(), id()));
        assert!(message.expects_reply());
        assert_eq!(message.id(), Some(&id()));
    }

    #[test]
    fn a_result_points_back_at_its_request() {
        let result = IrResult::new(id());
        let message = IrMessage::Result(result);
        assert_eq!(message.id(), Some(&id()));
        assert!(!message.expects_reply());
        assert!(message.method().is_none());
    }

    /// A parse error has no id, which is why `IrError::id` is an `Option`
    /// rather than a plain field.
    #[test]
    fn a_parse_error_may_have_no_id() {
        let error = IrError::new(None, IrErrorCode::ParseError, "bad json");
        let message = IrMessage::Error(error);
        assert!(message.id().is_none());
        assert!(!message.expects_reply());
    }

    #[test]
    fn an_error_with_an_id_points_back_at_its_request() {
        let error = IrError::new(Some(id()), IrErrorCode::InvalidParams, "nope");
        let message = IrMessage::Error(error);
        assert_eq!(message.id(), Some(&id()));
    }

    /// Every reserved JSON-RPC code, and the implementation range, round trip.
    #[test]
    fn error_codes_round_trip() {
        for code in [
            IrErrorCode::ParseError,
            IrErrorCode::InvalidRequest,
            IrErrorCode::MethodNotFound,
            IrErrorCode::InvalidParams,
            IrErrorCode::InternalError,
        ] {
            assert_eq!(IrErrorCode::from_code(code.code()), code, "{code:?}");
        }
        for raw in [-32000_i64, -32001, -32050, -32099] {
            let code = IrErrorCode::Implementation(raw);
            assert_eq!(code.code(), raw);
            assert_eq!(IrErrorCode::from_code(raw), code);
        }
        // The whole reserved range maps to `Implementation`, and nothing
        // outside it does.
        for raw in -32099_i64..=-32000 {
            assert!(
                matches!(IrErrorCode::from_code(raw), IrErrorCode::Implementation(_)),
                "{raw}",
            );
        }
    }

    #[test]
    fn a_code_outside_every_reserved_range_is_other() {
        for raw in [0_i64, 1, -1, -31999, 12345] {
            assert_eq!(
                IrErrorCode::from_code(raw),
                IrErrorCode::Other(raw),
                "{raw}",
            );
            assert_eq!(IrErrorCode::Other(raw).code(), raw);
        }
    }

    /// The envelope is not version-gated in any of its four shapes, so
    /// lowering never has to ask about the message kind.
    #[test]
    fn no_envelope_shape_requires_a_feature() {
        let messages = [
            IrMessage::Request(IrRequest::new(MethodName::new("ping").unwrap(), id())),
            IrMessage::Notification(IrNotification::new(
                MethodName::new("notifications/initialized").unwrap(),
            )),
            IrMessage::Result(IrResult::new(id())),
            IrMessage::Error(IrError::new(Some(id()), IrErrorCode::InternalError, "x")),
        ];
        for message in messages {
            assert!(message.required_features().is_empty(), "{message:?}");
        }
    }
}
