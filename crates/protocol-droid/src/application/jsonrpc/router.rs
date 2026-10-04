//! Turning a decoded envelope into a method to dispatch, without naming one.
//!
//! # The trait is the seam, and it is monomorphised on purpose
//!
//! [`MethodTable`] knows a method's *name* and can turn its raw params into a
//! typed value, but it is declared by the consumer and implemented by whichever
//! protocol module is in play. Nothing here mentions `tools/call` or
//! `initialize`; that is the whole point of the split, and it is what lets T27
//! and T29 add A2A and ACP without touching a line of this file.
//!
//! The trait is generic over its own `MethodId` and its own decoded params
//! rather than erasing them to `Box<dyn Debug>`. Erasing would put a heap
//! allocation on the path this whole layer exists to keep allocation-free, and
//! it would throw away the type the caller needed before anything could be
//! dispatched. The associated types are what "consumed through `impl
//! MethodTable` so it monomorphises" means in practice.
//!
//! # Three type parameters, because a reply carries nothing
//!
//! [`Routed`] has one type parameter per arm that holds a value: the request
//! params, the notification params, and nothing for the reply. A single
//! parameter would mean either erasing the two params types into one or
//! manufacturing a value the reply arm never reads, and both are worse than
//! carrying the third type. It is the same argument as not erasing to
//! `Box<dyn Debug>`, applied to the reply case.
//!
//! # Where an error is raised
//!
//! A method that is not in the table is `-32601` and a params shape that does
//! not fit is `-32602` — both decided here, from the same code, whether the
//! table is MCP's or A2A's. The message is the only part a protocol supplies.
//! That is the difference between "route" and "dispatch", and keeping the seam
//! between them is what lets a later task fill in the second half without
//! rewriting the first.

use serde_json::value::RawValue;

use crate::domain::feature::FeatureProfile;
use crate::domain::ir::IrErrorCode;
use crate::domain::jsonrpc::{Envelope, MessageKind, Method, Outcome, WireId, decode};

use super::BatchPolicy;

/// What a routed frame contains, for one table.
///
/// A reply carries no decoded params, which is the whole reason [`Routed`] has
/// three type parameters rather than one; naming the pair here keeps the two
/// public signatures readable rather than wrapping across four lines.
pub type RoutedFrame<'a, M> = Vec<RoutedMessage<'a, M>>;

/// The same, for a single message.
pub type RoutedMessage<'a, M> = Routed<
    'a,
    <M as MethodTable>::MethodId,
    <M as MethodTable>::RequestParams<'a>,
    <M as MethodTable>::NotificationParams<'a>,
>;

/// The set of methods one protocol speaks, and how to decode each one's params.
///
/// # Implementing this
///
/// A protocol module implements it once. [`lookup`](Self::lookup) is the
/// dispatch table and should be a `match` on the method name — the compiler
/// turns a `match` on string literals into a length switch followed by a memcmp
/// chain, which beats a hash table for a small, fixed vocabulary and allocates
/// nothing.
///
/// The two `decode_*_params` methods are separate rather than one method with an
/// `is_request` flag because a request and a notification of the same method
/// legitimately have different params, and a flag would push that decision out
/// to every call site.
///
/// ```
/// use protocol_droid::application::jsonrpc::{MethodTable, RouteError};
/// use serde_json::value::RawValue;
///
/// #[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// enum MyMethod {
///     Ping,
/// }
///
/// struct MyTable;
///
/// impl MethodTable for MyTable {
///     type MethodId = MyMethod;
///     type RequestParams<'a> = ();
///     type NotificationParams<'a> = ();
///
///     fn lookup(&self, name: &str) -> Option<MyMethod> {
///         match name {
///             "ping" => Some(MyMethod::Ping),
///             _ => None,
///         }
///     }
///
///     fn decode_request_params<'a>(
///         &self,
///         _method: MyMethod,
///         _params: Option<&'a RawValue>,
///     ) -> Result<(), RouteError<'a>> {
///         Ok(())
///     }
///
///     fn decode_notification_params<'a>(
///         &self,
///         _method: MyMethod,
///         _params: Option<&'a RawValue>,
///     ) -> Result<(), RouteError<'a>> {
///         Ok(())
///     }
/// }
/// ```
pub trait MethodTable {
    /// How this table names the methods it supports.
    type MethodId: Copy + std::fmt::Debug + PartialEq;

    /// The decoded params of a request, which are method- and revision-specific.
    ///
    /// A GAT rather than a plain associated type because the decoded value
    /// borrows the frame buffer. A protocol whose request params reference the
    /// id, or slice the raw JSON, would otherwise be forced to own a copy of
    /// data that is still alive.
    type RequestParams<'a>;

    /// The decoded params of a notification.
    type NotificationParams<'a>;

    /// The method a name denotes, or `None` if this protocol has no such
    /// method — which the router answers with `-32601`.
    fn lookup(&self, name: &str) -> Option<Self::MethodId>;

    /// Decodes a request's params.
    ///
    /// # Errors
    ///
    /// [`RouteError::InvalidParams`] if `params` does not fit the method.
    fn decode_request_params<'a>(
        &self,
        method: Self::MethodId,
        params: Option<&'a RawValue>,
    ) -> Result<Self::RequestParams<'a>, RouteError<'a>>;

    /// Decodes a notification's params.
    ///
    /// # Errors
    ///
    /// [`RouteError::InvalidParams`] if `params` does not fit the method.
    fn decode_notification_params<'a>(
        &self,
        method: Self::MethodId,
        params: Option<&'a RawValue>,
    ) -> Result<Self::NotificationParams<'a>, RouteError<'a>>;
}

/// A routed message: the method it names and its params, already typed.
///
/// `R` and `N` are the request and notification params, which are different
/// types because a request and a notification of the same method legitimately
/// have different params. The reply arm carries neither, which is why there is
/// no parameter for it.
#[derive(Debug, Clone)]
pub enum Routed<'a, M, R, N> {
    /// A request, which expects a reply carrying this id.
    Request {
        /// The method the sender named.
        method: M,
        /// The id to answer with.
        id: WireId<'a>,
        /// The decoded params.
        params: R,
    },
    /// A notification, which expects nothing.
    Notification {
        /// The method the sender named.
        method: M,
        /// The decoded params.
        params: N,
    },
    /// A reply, passed through without routing.
    ///
    /// Carried rather than dropped because a peer that sent us a result is a
    /// peer whose response we were waiting for, and the session engine decides
    /// what to match it against. Discarding it here would push that decision
    /// down into the transport.
    Reply {
        /// The id the reply answers.
        id: WireId<'a>,
        /// The reply payload, undecoded.
        payload: ReplyPayload<'a>,
    },
}

impl<M, R, N> Routed<'_, M, R, N> {
    /// The method, for the two shapes that have one.
    #[must_use]
    pub const fn method(&self) -> Option<M>
    where
        M: Copy,
    {
        match self {
            Self::Request { method, .. } | Self::Notification { method, .. } => Some(*method),
            Self::Reply { .. } => None,
        }
    }
}

/// The undecoded half of a reply.
///
/// No `PartialEq`, for the reason on [`WireError`](crate::domain::jsonrpc::WireError).
#[derive(Debug, Clone)]
pub enum ReplyPayload<'a> {
    /// A `result` member.
    Result(&'a RawValue),
    /// An `error` member, as its code and message.
    Error {
        /// The numeric code the sender used.
        code: i64,
        /// The human-readable message.
        message: &'a str,
    },
}

/// Why a frame was refused.
///
/// # Why the method name is borrowed
///
/// The name is borrowed from the frame rather than interned, because a table
/// of "known" names is a second list that can disagree with the method table,
/// and a disagreement here is silent: a legitimate method name that the intern
/// list has not heard of would be reported to the operator as `unknown`, which
/// is exactly the case an operator most needs to see. The frame outlives the
/// error — the error is produced while reading it — so the borrow is free.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteError<'a> {
    /// The frame was not decodable at all.
    #[error(transparent)]
    Decode(#[from] crate::domain::jsonrpc::DecodeError),
    /// A batch arrived and this session does not accept one (`-32600`).
    ///
    /// `InvalidRequest` rather than a code of its own: the specification has no
    /// batch-specific code, and inventing one would make a peer that switches
    /// on codes meet something it has never heard of.
    #[error("this session does not accept batch frames")]
    BatchNotAllowed,
    /// The method is not in this protocol's table (`-32601`).
    #[error("unknown method `{0}`")]
    MethodNotFound(Method<'a>),
    /// The params did not fit the method (`-32602`).
    #[error("invalid params for `{method}`: {reason}")]
    InvalidParams {
        /// The method whose params did not fit.
        method: Method<'a>,
        /// Why.
        reason: &'static str,
    },
}

impl RouteError<'_> {
    /// The JSON-RPC code this failure is reported as.
    ///
    /// The batch case answers `InvalidRequest` and the unknown-method case
    /// answers `MethodNotFound`; both are the codes the specification gives,
    /// and reporting a batch refusal as `-32601` would tell the peer the wrong
    /// thing about which half of the problem to fix.
    #[must_use]
    pub const fn code(&self) -> IrErrorCode {
        match self {
            Self::Decode(decode) => decode.code(),
            Self::BatchNotAllowed | Self::InvalidParams { .. } => IrErrorCode::InvalidRequest,
            Self::MethodNotFound(_) => IrErrorCode::MethodNotFound,
        }
    }
}

/// Routes one message, decoding its params into the table's own types.
///
/// # Errors
///
/// [`RouteError::MethodNotFound`] if the table has no such method, and
/// [`RouteError::InvalidParams`] if the params do not fit. A reply never
/// errors — it is passed through, because interpreting it is the session
/// engine's job rather than this layer's.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::jsonrpc::{MethodTable, RouteError, Routed, route_one};
/// use protocol_droid::domain::jsonrpc::{Frame, decode};
/// use serde_json::value::RawValue;
///
/// # #[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// # enum Ping { Method }
/// # struct Table;
/// # impl MethodTable for Table {
/// #     type MethodId = Ping;
/// #     type RequestParams<'a> = ();
/// #     type NotificationParams<'a> = ();
/// #     fn lookup(&self, name: &str) -> Option<Ping> { (name == "ping").then_some(Ping::Method) }
/// #     fn decode_request_params<'a>(&self, _: Ping, _: Option<&'a RawValue>) -> Result<(), RouteError<'a>> { Ok(()) }
/// #     fn decode_notification_params<'a>(&self, _: Ping, _: Option<&'a RawValue>) -> Result<(), RouteError<'a>> { Ok(()) }
/// # }
/// let frame = decode(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
///     .expect("a well-formed request decodes");
/// let Frame::Single(envelope) = frame else {
///     panic!("a lone object is not a batch");
/// };
/// let routed = route_one(envelope, &Table).expect("ping is in the table");
/// assert!(matches!(routed, Routed::Request { .. }));
/// ```
///
/// Takes the envelope by value: a routed request owns the id it must answer
/// with, and an id whose text needed unescaping owns a `String`. Borrowing
/// would force a clone on exactly the ids where cloning costs, so the envelope
/// is moved instead — the caller that is iterating a frame is iterating owned
/// envelopes anyway.
pub fn route_one<'a, M: MethodTable>(
    envelope: Envelope<'a>,
    table: &M,
) -> Result<RoutedMessage<'a, M>, RouteError<'a>> {
    let Envelope {
        kind,
        method,
        params,
        id,
        outcome,
    } = envelope;
    let method = method.unwrap_or_default();
    match kind {
        MessageKind::Request => {
            let Some(id) = id else {
                return Err(RouteError::InvalidParams {
                    method,
                    reason: "a request must carry an id",
                });
            };
            let Some(found) = table.lookup(method.as_ref()) else {
                return Err(RouteError::MethodNotFound(method));
            };
            let params = table.decode_request_params(found, params)?;
            Ok(Routed::Request {
                method: found,
                id,
                params,
            })
        }
        MessageKind::Notification => {
            let Some(found) = table.lookup(method.as_ref()) else {
                return Err(RouteError::MethodNotFound(method));
            };
            let params = table.decode_notification_params(found, params)?;
            Ok(Routed::Notification {
                method: found,
                params,
            })
        }
        MessageKind::Result | MessageKind::Error => {
            let Some(id) = id else {
                return Err(RouteError::InvalidParams {
                    method: Method::Borrowed("a reply"),
                    reason: "must carry an id",
                });
            };
            let payload = match outcome {
                Some(Outcome::Result(value)) => ReplyPayload::Result(value),
                Some(Outcome::Error(error)) => ReplyPayload::Error {
                    code: error.code,
                    message: error.message,
                },
                // `decode` refuses a message with neither, so this arm is
                // unreachable rather than reachable. The alternative is an
                // error a peer can trigger, which is worse than either.
                None => {
                    return Err(RouteError::InvalidParams {
                        method: Method::Borrowed("a reply"),
                        reason: "must carry a result or an error",
                    });
                }
            };
            Ok(Routed::Reply { id, payload })
        }
    }
}

/// Decodes a frame and routes every message in it, refusing a batch when
/// `policy` says this session does not accept one.
///
/// # Errors
///
/// [`RouteError::BatchNotAllowed`] for a batch the policy refuses, and
/// whatever [`route_one`] returns for the first message that cannot be routed.
///
/// A batch is all-or-nothing on the way in. The specification lets a server
/// answer each element independently, but it cannot answer any of them before
/// the whole frame is decoded, so a bad element is found before a single
/// handler runs. That is also the only reading that keeps a refusal from
/// having run half the batch.
pub fn route_frame<'a, M: MethodTable, P: BatchPolicy>(
    input: &'a [u8],
    table: &M,
    profile: &FeatureProfile,
    policy: &P,
) -> Result<RoutedFrame<'a, M>, RouteError<'a>> {
    let frame = decode(input)?;
    if frame.is_batch() && !policy.allows_batch(profile) {
        return Err(RouteError::BatchNotAllowed);
    }
    frame
        .into_envelopes()
        .into_iter()
        .map(|envelope| route_one(envelope, table))
        .collect()
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
    //! The router, with a stand-in table.
    //!
    //! # Why a stand-in rather than MCP's real table
    //!
    //! `application` may not name an adapter — that is the hexagonal rule, and
    //! a test that breaks it teaches the wrong lesson. So the table here is
    //! defined in this module with two methods and no protocol meaning. The
    //! real MCP table is tested against this same router in
    //! `adapters/secondary/mcp/method_table.rs`, which is the one place the
    //! dependency is allowed to point.
    //!
    //! These tests live in the module because a dependent crate in `tests/`
    //! cannot keep the compiler from calling a public-but-unused item dead
    //! when it builds this crate as a test binary. `tests/jsonrpc_router.rs`
    //! still exists as the cross-layer check; this is the unit check.

    use assert_matches::assert_matches;
    use serde_json::value::RawValue;

    use crate::domain::feature::FeatureProfile;
    use crate::domain::ir::IrErrorCode;
    use crate::domain::jsonrpc::WireId;

    use super::{MethodTable, ReplyPayload, RouteError, Routed, route_frame, route_one};
    use crate::application::jsonrpc::{AllowBatching, DenyBatching, ProfileBatching};

    type TestError = Box<dyn std::error::Error>;
    type Params<'a> = Option<&'a RawValue>;

    /// Two methods with no protocol meaning, so a test cannot pass by
    /// accident on a name the specification happens to define.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestMethod {
        Alpha,
        Beta,
    }

    /// A table that only knows two methods, and that records whether the
    /// params decoder ran — the point of the whole "decode after routing"
    /// arrangement is that an unroutable frame never pays for it.
    struct TestTable {
        decoded: std::cell::Cell<usize>,
    }

    impl TestTable {
        const fn new() -> Self {
            Self {
                decoded: std::cell::Cell::new(0),
            }
        }
    }

    impl MethodTable for TestTable {
        type MethodId = TestMethod;
        type RequestParams<'a> = Params<'a>;
        type NotificationParams<'a> = Params<'a>;

        fn lookup(&self, name: &str) -> Option<TestMethod> {
            match name {
                "alpha" => Some(TestMethod::Alpha),
                "beta" => Some(TestMethod::Beta),
                _ => None,
            }
        }

        fn decode_request_params<'a>(
            &self,
            _method: TestMethod,
            params: Option<&'a RawValue>,
        ) -> Result<Self::RequestParams<'a>, RouteError<'a>> {
            self.decoded.set(self.decoded.get() + 1);
            Ok(params)
        }

        fn decode_notification_params<'a>(
            &self,
            _method: TestMethod,
            params: Option<&'a RawValue>,
        ) -> Result<Self::NotificationParams<'a>, RouteError<'a>> {
            self.decoded.set(self.decoded.get() + 1);
            Ok(params)
        }
    }

    /// A batch, as a peer would send it.
    const BATCH: &[u8] =
        br#"[{"jsonrpc":"2.0","id":1,"method":"alpha"},{"jsonrpc":"2.0","id":2,"method":"beta"}]"#;

    #[test]
    fn a_request_reports_its_method_through_the_accessor() {
        let table = TestTable::new();
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"alpha"}"#;
        let routed = route_one(single(input), &table).expect("alpha is in the table");
        assert_eq!(routed.method(), Some(TestMethod::Alpha));
    }

    #[test]
    fn a_reply_reports_no_method() {
        let table = TestTable::new();
        let input = br#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let routed = route_one(single(input), &table).expect("a reply routes to no method");
        assert_eq!(routed.method(), None, "a reply has no method to name");
    }

    #[test]
    fn an_unroutable_frame_never_decodes_its_params() {
        // The whole reason params decoding is lazy: a frame whose method does
        // not exist is refused on the name alone, and the params are never
        // looked at. If this fails, a peer can make the server parse arbitrary
        // JSON by naming a method that does not exist.
        let table = TestTable::new();
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"nope","params":{"deep":[1,2,3]}}"#;
        let error = route_one(single(input), &table).expect_err("no such method");
        assert_eq!(error.code(), IrErrorCode::MethodNotFound);
        assert_eq!(table.decoded.get(), 0, "params decoding must not have run");
    }

    #[test]
    fn a_routable_frame_decodes_its_params_exactly_once() {
        let table = TestTable::new();
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"alpha","params":{"x":1}}"#;
        route_one(single(input), &table).expect("alpha is in the table");
        assert_eq!(table.decoded.get(), 1);
    }

    #[test]
    fn a_batch_routes_every_element_in_order() {
        let table = TestTable::new();
        let routed = route_frame(BATCH, &table, &FeatureProfile::all(), &ProfileBatching)
            .expect("batching is allowed in this profile");
        let methods: Vec<_> = routed.iter().filter_map(Routed::method).collect();
        assert_eq!(methods, [TestMethod::Alpha, TestMethod::Beta]);
    }

    #[test]
    fn a_decode_failure_outranks_the_batch_policy() {
        // A malformed frame is malformed whether or not batching is on. If
        // the policy were checked first, a session with batching enabled would
        // report `-32700` as `-32600` and a peer could not tell a broken
        // frame from a refused one.
        let table = TestTable::new();
        let error = route_frame(
            b"{not json",
            &table,
            &FeatureProfile::all(),
            &ProfileBatching,
        )
        .expect_err("the frame is malformed");
        assert_eq!(error.code(), IrErrorCode::ParseError);
    }

    #[test]
    fn a_single_message_is_unaffected_by_the_batch_policy() {
        // Refusing batches must not refuse messages, or a server that has
        // turned batching off answers nothing at all.
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"alpha"}"#;
        // Two calls rather than a loop: the two policies are different types,
        // and a `[DenyBatching, AllowBatching]` array would have to erase them
        // to a trait object to iterate — which is the allocation this whole
        // layer exists to avoid.
        let table = TestTable::new();
        let routed = route_frame(input, &table, &FeatureProfile::empty(), &DenyBatching)
            .expect("a single message is not a batch");
        assert_eq!(
            routed.len(),
            1,
            "DenyBatching must not refuse a single message"
        );

        let table = TestTable::new();
        let routed = route_frame(input, &table, &FeatureProfile::empty(), &AllowBatching)
            .expect("a single message is not a batch");
        assert_eq!(
            routed.len(),
            1,
            "AllowBatching must not refuse a single message"
        );
    }

    #[test]
    fn the_profile_decides_whether_a_batch_is_allowed() {
        let table = TestTable::new();
        let error = route_frame(BATCH, &table, &FeatureProfile::empty(), &ProfileBatching)
            .expect_err("this profile has no batching");
        assert_eq!(
            error.code(),
            IrErrorCode::InvalidRequest,
            "an unacceptable batch is `-32600`, not a code of its own"
        );
    }

    #[test]
    fn a_policy_that_ignores_the_profile_overrides_it_both_ways() {
        // `AllowBatching` and `DenyBatching` exist for the conformance runner,
        // which has to exercise both behaviours regardless of what a profile
        // says. If they were not genuinely unconditional, the runner would be
        // testing the profile and calling it a policy.
        let table = TestTable::new();
        assert!(
            route_frame(BATCH, &table, &FeatureProfile::empty(), &AllowBatching).is_ok(),
            "AllowBatching must allow a batch the profile refuses"
        );
        let table = TestTable::new();
        assert!(
            route_frame(BATCH, &table, &FeatureProfile::all(), &DenyBatching).is_err(),
            "DenyBatching must refuse a batch the profile allows"
        );
    }

    #[test]
    fn a_null_id_is_a_malformed_frame_not_a_notification() {
        // The case the decoder's `present_member` exists for. JSON-RPC treats
        // an absent `id` as "this is a notification"; a peer that sends
        // `"id": null` is not saying that, it is sending an id that is not a
        // number or a string. Collapsing the two would make the request
        // unanswerable with no error at all, so the decoder keeps them
        // distinct and refuses this one as `-32600`.
        let input = br#"{"jsonrpc":"2.0","id":null,"method":"alpha"}"#;
        let error = crate::domain::jsonrpc::decode(input).expect_err("a null id is not a valid id");
        assert_eq!(error.code(), IrErrorCode::InvalidRequest);
        assert!(
            error.to_string().contains("number or a string"),
            "and says why: {error}"
        );
    }

    #[test]
    fn a_message_carrying_neither_method_nor_reply_is_refused_before_routing() {
        // Refused by the decoder, not the router. That ordering matters: an
        // `id` with no `method` and no `result` is not a request for an
        // unknown method, so `-32601` would be a lie, and it never reaches a
        // table at all.
        let input = br#"{"jsonrpc":"2.0","id":1}"#;
        let error = crate::domain::jsonrpc::decode(input)
            .expect_err("a message must be a request, a notification, or a reply");
        assert_eq!(error.code(), IrErrorCode::InvalidRequest);
        assert!(
            error.to_string().contains("method or a reply"),
            "and says which half is missing: {error}"
        );
    }

    #[test]
    fn a_null_result_is_a_result() -> Result<(), TestError> {
        // The mirror of the `id` case, and the reason `result` also uses
        // `present_member`. `"result": null` is a real result, not an absent
        // one, and MCP sends it — a `tools/call` with empty content, a handler
        // that returns nothing. Read as absent it looks like a reply with no
        // payload, and the peer is told `-32600` for sending a valid message.
        let table = TestTable::new();
        let input = br#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        match route_one(single(input), &table) {
            Ok(Routed::Reply { id, payload }) => {
                assert_eq!(id, WireId::Number(1));
                assert_matches!(payload, ReplyPayload::Result(raw) if raw.get() == "null");
            }
            other => return Err(format!("expected a reply, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn an_error_reply_keeps_its_code_and_message() -> Result<(), TestError> {
        let table = TestTable::new();
        let input = br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#;
        match route_one(single(input), &table) {
            Ok(Routed::Reply { id, payload }) => {
                assert_eq!(id, WireId::Number(1));
                assert_matches!(
                    payload,
                    ReplyPayload::Error { code, message }
                        if code == -32601 && message == "nope"
                );
            }
            other => return Err(format!("expected a reply, got {other:?}").into()),
        }
        Ok(())
    }

    /// The router takes an owned envelope, so the tests need the single-message
    /// unwrap that `route_frame` does for them.
    fn single(input: &[u8]) -> crate::domain::jsonrpc::Envelope<'_> {
        match crate::domain::jsonrpc::decode(input) {
            Ok(crate::domain::jsonrpc::Frame::Single(envelope)) => envelope,
            // Both arms are a test-authoring mistake, not a protocol outcome:
            // every caller above passes one well-formed object. A panic here
            // is the honest signal for that, and it is in test code.
            Ok(crate::domain::jsonrpc::Frame::Batch(_)) => {
                panic!("these tests each pass exactly one message")
            }
            Err(error) => panic!("these tests each pass well-formed JSON: {error}"),
        }
    }
}
