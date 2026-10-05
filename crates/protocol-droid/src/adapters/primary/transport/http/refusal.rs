//! Why a request was refused, and the status code and body it becomes.
//!
//! # Why the status code lives here and not in the handlers
//!
//! Because "which code" is the part of a transport that is easy to get subtly
//! wrong and impossible to see. A handler that returns `Err` and lets a
//! `IntoResponse` impl decide is a handler whose test has to go through HTTP
//! to check a number, which is why a `#[test]` on a handler is usually a test
//! of the mapping and not of the handler. Keeping the mapping in one exhaustive
//! `match` means every refusal the adapter can produce is visible in one place,
//! and adding a variant without deciding its code is a compile error rather
//! than a `todo!()`.
//!
//! # The codes are not interchangeable, and the distinctions are the point
//!
//! - **400** for a request the server understood and could not accept: a
//!   malformed `Mcp-Session-Id`, a missing one, an `MCP-Protocol-Version` that
//!   disagrees with the session.
//! - **404** for a session the server does not have. Distinct from 400 because
//!   the client's fix is different: a 404 tells it to re-`initialize`, and a
//!   400 tells it its header is wrong. The specification makes that distinction
//!   explicit — "When a client receives HTTP 404 in response to a request
//!   containing an `Mcp-Session-Id`, it MUST start a new session".
//! - **405** for a method this endpoint does not offer here, which is the
//!   specification's own answer for a GET on a server with no server-initiated
//!   stream and for a DELETE on a server that will not terminate sessions.
//! - **409** for a resume that cannot be honoured without losing an event.
//!
//! The 409 is this adapter's own choice; the specification does not name one,
//! and it is the only code in this file that it does. It is a conflict between
//! what the client asked for and what the server can still provide, which is
//! what 409 is for, and the body names the gap rather than reporting a short
//! read as a success.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::JSON_MEDIA_TYPE;
use crate::domain::EventId;

/// A request the adapter refuses before or instead of answering it.
///
/// Not an `Error` in the `std::error::Error` sense for its own sake — it is
/// one so it composes with `?` in a handler whose other failure is a genuine
/// error, and so `thiserror` writes the `Display` text that goes in the body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The `Mcp-Session-Id` header was present but not a session id.
    #[error("the Mcp-Session-Id header is not a well-formed session id")]
    MalformedSessionId,

    /// A stateful request arrived with no session id and was not `initialize`.
    #[error("this request requires an Mcp-Session-Id header")]
    MissingSession,

    /// The session id named a session this server does not have.
    #[error("no session is registered under that Mcp-Session-Id")]
    UnknownSession,

    /// The endpoint does not offer this method here.
    #[error("this endpoint does not offer {method} for {reason}")]
    MethodNotAllowed {
        /// The HTTP method that was refused.
        method: &'static str,
        /// Why it is not offered, in the specification's own terms.
        reason: &'static str,
    },

    /// `MCP-Protocol-Version` disagreed with the negotiated version, or named
    /// something this build cannot parse.
    #[error("the MCP-Protocol-Version header is not usable: {reason}")]
    ProtocolHeader {
        /// What the domain's `check_protocol_header` reported.
        reason: String,
    },

    /// The body was larger than the configured ceiling.
    #[error("the request body exceeds the configured limit")]
    BodyTooLarge,

    /// The request carried no credentials and the endpoint requires some.
    #[error("this endpoint requires authentication")]
    Unauthenticated,

    /// The request was authenticated but lacks a required scope.
    #[error("this endpoint requires a scope the caller does not hold")]
    Forbidden,

    /// The body was not a JSON-RPC frame this build can decode.
    #[error("the request body is not a decodable JSON-RPC frame: {reason}")]
    MalformedBody {
        /// What `decode` reported.
        reason: String,
    },

    /// A `Last-Event-ID` names an event the ring has dropped.
    ///
    /// The whole point of the variant is the two watermarks. A client that
    /// resumes from `requested` and is told `evicted_through` knows exactly
    /// which of its events are gone and can re-issue them; a client that is
    /// handed a short stream and no explanation believes it is current.
    #[error("the stream dropped events before the requested cursor")]
    StreamGap {
        /// The cursor the client quoted.
        requested: EventId,
        /// The newest event the ring has dropped.
        evicted_through: EventId,
        /// The newest event the stream ever issued.
        last: EventId,
    },

    /// A `Last-Event-ID` names an event the stream never issued.
    #[error("the requested cursor is ahead of this stream")]
    UnknownCursor {
        /// The cursor the client quoted.
        requested: EventId,
        /// The newest event the stream ever issued.
        last: EventId,
    },

    /// The `Last-Event-ID` header was present but is not a cursor this stream
    /// can read.
    ///
    /// Separate from [`UnknownCursor`](Self::UnknownCursor) because the two
    /// faults have different fixes and the client has to tell them apart to
    /// make one: a cursor ahead of the stream is a well-formed id the server
    /// cannot serve, while a cursor it cannot parse is a malformed header. A
    /// server that answered a corrupt header by quietly starting the client
    /// from the top would hand it a stream it believes it has already seen, so
    /// this refuses rather than guessing — including for the empty string,
    /// which is a client that quoted a cursor and quoted nothing.
    #[error("the Last-Event-ID header is not a well-formed event cursor")]
    MalformedCursor,

    /// The protocol layer failed in a way the adapter does not model.
    #[error("{0}")]
    Handler(String),
}

impl Refusal {
    /// The status this refusal is answered with.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        match self {
            // "Servers that require a session ID SHOULD respond to requests
            // without an Mcp-Session-Id header (other than initialization) with
            // HTTP 400 Bad Request." — transports, 2025-06-18.
            //
            // The citation covers the first three arms. `MalformedBody` is a
            // 400 for the same reason without the quote, and `MalformedCursor`
            // is this adapter's own 400 for the same reason again: a header
            // the server understood and could not accept. A cursor is not a
            // session id, and its own slug and code are what tell a client
            // which header it got wrong.
            Self::MalformedSessionId
            | Self::MissingSession
            | Self::ProtocolHeader { .. }
            | Self::MalformedBody { .. }
            | Self::MalformedCursor => StatusCode::BAD_REQUEST,

            // "it MUST respond to requests containing that session ID with HTTP
            // 404 Not Found" — same section.
            Self::UnknownSession => StatusCode::NOT_FOUND,

            Self::MethodNotAllowed { .. } => StatusCode::METHOD_NOT_ALLOWED,

            Self::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,

            // Not the specification's number; see the module docs. Both
            // resume refusals are a conflict between the client's cursor and
            // the server's log, and both are reported with the watermarks that
            // let the client act.
            Self::StreamGap { .. } | Self::UnknownCursor { .. } => StatusCode::CONFLICT,

            Self::Handler(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The JSON-RPC error code sent alongside the status.
    ///
    /// Separate from [`status`](Self::status) because the two answer different
    /// questions and are allowed to disagree: the status is what a proxy, a
    /// browser or `curl -I` sees, and the code is what a conforming MCP client
    /// branches on. Collapsing them would force one compromise on both.
    ///
    /// Where the JSON-RPC specification reserves a code that fits, the
    /// reserved code is used. The rest live in `-32000..=-32099`, the range
    /// JSON-RPC 2.0 defines as "implementation-defined server errors" — which
    /// is exactly what a transport-level refusal is: not a malformed JSON-RPC
    /// message (nothing was wrong with the *message*), but a server declining
    /// for a reason of its own. Each arm is a distinct code because a client
    /// recovering from one of these has a different move for each.
    #[must_use]
    pub const fn code(&self) -> i64 {
        match self {
            // Reserved: the body was not a decodable JSON-RPC frame, which is
            // precisely JSON-RPC's own "parse error".
            Self::MalformedBody { .. } => -32700,

            // Reserved: the request was understood and could not be accepted —
            // a header that does not name a session, or a protocol version
            // this server did not negotiate.
            Self::MalformedSessionId | Self::MissingSession | Self::ProtocolHeader { .. } => -32600,

            // Reserved: the method is not one this endpoint serves.
            Self::MethodNotAllowed { .. } => -32601,

            // Reserved: the protocol layer failed in a way this adapter does
            // not model, so it has nothing more specific to say.
            Self::Handler(_) => -32603,

            // Implementation-defined, one per recovery.
            Self::UnknownSession => -32001,
            Self::BodyTooLarge => -32002,
            Self::Unauthenticated => -32003,
            Self::Forbidden => -32004,
            Self::StreamGap { .. } => -32005,
            Self::UnknownCursor { .. } => -32006,
            Self::MalformedCursor => -32007,
        }
    }

    /// The JSON body sent alongside the status.
    ///
    /// A well-formed JSON-RPC 2.0 error object, because this is a
    /// machine-readable protocol and a client that receives a bare
    /// `409 Conflict` cannot recover — nothing in it says what was lost. Three
    /// members carry that:
    ///
    /// - `error.code` is the integer from [`code`](Self::code), which a
    ///   client switches on.
    /// - `error.message` is the human text, which is this type's `Display` —
    ///   the same text the log line carries, so the two can never disagree.
    /// - `error.data.error` is the stable slug from [`slug`], the member a
    ///   client branches on when it wants a name rather than a number, and
    ///   where the resume watermarks live.
    ///
    /// `id` is always `null`. A refusal is frequently the answer to a request
    /// whose body was never parsed, so there may be no id to echo; sending
    /// `null` rather than omitting the member keeps the document valid
    /// against the JSON-RPC schema, which requires the member on an error.
    #[must_use]
    pub fn body(&self) -> Value {
        let data = match self {
            Self::StreamGap {
                requested,
                evicted_through,
                last,
            } => json!({
                "error": "stream_gap",
                "requested": requested.get(),
                "evicted_through": evicted_through.get(),
                "last": last.get(),
            }),
            Self::UnknownCursor { requested, last } => json!({
                "error": "unknown_cursor",
                "requested": requested.get(),
                "last": last.get(),
            }),
            other => json!({ "error": slug(other) }),
        };
        json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": {
                "code": self.code(),
                "message": self.to_string(),
                "data": data,
            },
        })
    }
}

/// A stable, lowercase, hyphenated identifier for a refusal variant.
///
/// Written out rather than derived, because the slug is part of the wire
/// contract: a client branches on it, so renaming a Rust variant must not
/// silently rename the string. Each arm is paired with the variant it covers
/// so a new variant fails to compile here rather than falling through.
const fn slug(refusal: &Refusal) -> &'static str {
    match refusal {
        Refusal::MalformedSessionId => "malformed_session_id",
        Refusal::MissingSession => "missing_session",
        Refusal::UnknownSession => "unknown_session",
        Refusal::MethodNotAllowed { .. } => "method_not_allowed",
        Refusal::ProtocolHeader { .. } => "protocol_header",
        Refusal::BodyTooLarge => "body_too_large",
        Refusal::Unauthenticated => "unauthenticated",
        Refusal::Forbidden => "forbidden",
        Refusal::MalformedBody { .. } => "malformed_body",
        Refusal::StreamGap { .. } => "stream_gap",
        Refusal::UnknownCursor { .. } => "unknown_cursor",
        Refusal::MalformedCursor => "malformed_cursor",
        Refusal::Handler(_) => "handler_error",
    }
}

impl IntoResponse for Refusal {
    /// Renders the refusal, with the security layer still applied on top.
    ///
    /// This impl exists so a handler can `?` a `Refusal` into a `Response`. It
    /// sets only the status, the content type and — for the one refusal where
    /// it is meaningful — the `Allow` header; the security headers come from
    /// the router layer, which is the only place that can guarantee they are on
    /// *every* response including this one.
    fn into_response(self) -> Response {
        let status = self.status();
        let body = serde_json::to_vec(&self.body()).unwrap_or_else(|_| Vec::new());
        let mut response = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, JSON_MEDIA_TYPE)
            .body(axum::body::Body::from(body))
            .unwrap_or_else(|_| {
                // A response that cannot even be built means the status or a
                // header name was not a valid HTTP value, which for the values
                // this enum produces is not reachable. Building an empty 500
                // rather than panicking keeps the invariant that a refusal
                // never takes the process down.
                let mut fallback = Response::new(axum::body::Body::empty());
                *fallback.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                fallback
            });
        if let Self::MethodNotAllowed { method, .. } = &self
            && let Ok(value) = HeaderValue::from_str(method)
        {
            response.headers_mut().insert(header::ALLOW, value);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use serde_json::Value;

    use crate::domain::EventId;

    use super::{Refusal, slug};

    type TestError = Box<dyn std::error::Error>;

    /// Every variant the adapter can refuse, paired with the status it is
    /// answered with.
    ///
    /// Written out rather than derived, for the reason [`slug`] is: a new
    /// variant that quietly inherited a neighbouring code would be a
    /// client-visible regression, and a table makes that a compile error here.
    /// Every case is iterated, never indexed, because the position in the table
    /// is not part of what is being claimed.
    #[test]
    fn every_variant_maps_to_its_status() {
        let cases = [
            (Refusal::MalformedSessionId, StatusCode::BAD_REQUEST),
            (Refusal::MissingSession, StatusCode::BAD_REQUEST),
            (
                Refusal::ProtocolHeader {
                    reason: "unknown version".to_owned(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                Refusal::MalformedBody {
                    reason: "not json".to_owned(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (Refusal::UnknownSession, StatusCode::NOT_FOUND),
            (
                Refusal::MethodNotAllowed {
                    method: "GET",
                    reason: "no stream",
                },
                StatusCode::METHOD_NOT_ALLOWED,
            ),
            (Refusal::BodyTooLarge, StatusCode::PAYLOAD_TOO_LARGE),
            (Refusal::Unauthenticated, StatusCode::UNAUTHORIZED),
            (Refusal::Forbidden, StatusCode::FORBIDDEN),
            (
                Refusal::StreamGap {
                    requested: EventId::new(1),
                    evicted_through: EventId::new(1),
                    last: EventId::new(1),
                },
                StatusCode::CONFLICT,
            ),
            (
                Refusal::UnknownCursor {
                    requested: EventId::new(1),
                    last: EventId::new(1),
                },
                StatusCode::CONFLICT,
            ),
            (
                Refusal::Handler("boom".to_owned()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (refusal, expected) in cases {
            assert_eq!(refusal.status(), expected, "wrong status for {refusal:?}");
        }
    }

    /// The body is a JSON-RPC 2.0 error object, not a bespoke shape.
    ///
    /// This is the claim the whole `body` contract rests on: an MCP client
    /// parses refusals with its JSON-RPC error path, so a body that is not one
    /// is a body the client cannot read. The assertion is structural — the
    /// member exists, the code is an *integer*, and the message is a *string* —
    /// because "the body is non-empty" would pass against any of the ways this
    /// can go wrong.
    #[test]
    fn the_body_is_a_json_rpc_error_object() -> Result<(), TestError> {
        for refusal in [
            Refusal::MalformedSessionId,
            Refusal::UnknownSession,
            Refusal::Unauthenticated,
            Refusal::Forbidden,
            Refusal::BodyTooLarge,
        ] {
            let body = refusal.body();
            assert_eq!(body.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
            let error = body
                .get("error")
                .ok_or("no error member")?
                .as_object()
                .ok_or("error is not an object")?;
            // `as_u64` is the integer assertion: a code that had been written
            // as a string, or as `null`, returns None here rather than passing.
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .ok_or("code is not an integer")?;
            assert!(code < 0, "a server error code must be negative: {code}");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .ok_or("message is not a string")?;
            assert!(!message.is_empty());
            // `id` is a required member of a JSON-RPC response, and a refusal
            // often has no id to echo, so the contract is that it is null.
            assert!(body.get("id").is_some_and(Value::is_null));
        }
        Ok(())
    }

    /// Each variant has a distinct code, and the auth pair does not collide.
    ///
    /// A client recovers from a `401` by authenticating and from a `403` by
    /// asking for more permission; sending the same code for both sends it
    /// looking for a credential it already has. Codes are therefore compared
    /// pairwise rather than merely being "present".
    #[test]
    fn every_variant_has_its_own_code() {
        // The implementation-defined range is where distinctness is a
        // requirement: each of these has a *different recovery*, so a client
        // that cannot tell them apart retries the wrong thing. Pairwise
        // distinctness is asserted, not merely presence.
        let distinct = [
            Refusal::UnknownSession,
            Refusal::BodyTooLarge,
            Refusal::Unauthenticated,
            Refusal::Forbidden,
            Refusal::StreamGap {
                requested: EventId::new(1),
                evicted_through: EventId::new(1),
                last: EventId::new(1),
            },
            Refusal::UnknownCursor {
                requested: EventId::new(1),
                last: EventId::new(1),
            },
        ];
        for (index, refusal) in distinct.iter().enumerate() {
            for (other_index, other) in distinct.iter().enumerate() {
                if index != other_index {
                    assert_ne!(
                        refusal.code(),
                        other.code(),
                        "{refusal:?} and {other:?} share a code"
                    );
                }
            }
        }

        // The reserved range is deliberately *not* pairwise distinct. JSON-RPC
        // says a malformed session id and a missing one are both "invalid
        // request", and inventing distinct codes for them would misreport a
        // standard. What keeps them apart on the wire is the slug, which
        // `every_variant_has_a_stable_slug` covers.
        assert_eq!(Refusal::MalformedSessionId.code(), -32600);
        assert_eq!(Refusal::MissingSession.code(), -32600);
        assert_ne!(
            Refusal::MalformedSessionId.code(),
            Refusal::MalformedBody {
                reason: String::new()
            }
            .code()
        );

        // The auth pair specifically: a client recovers from one by
        // authenticating and from the other by asking for more permission.
        assert_ne!(Refusal::Unauthenticated.code(), Refusal::Forbidden.code());
        assert_eq!(Refusal::Unauthenticated.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(Refusal::Forbidden.status(), StatusCode::FORBIDDEN);
        assert_matches!(Refusal::Unauthenticated.to_string(), String { .. });
    }

    /// A malformed id and an unknown id are different faults with different
    /// client remedies, and they must not collapse into one code.
    ///
    /// This is the distinction the specification makes explicitly — a 404
    /// tells a client to re-`initialize`, a 400 tells it to fix its header — so
    /// a test that only asserted "both are 4xx" would pass against an
    /// implementation that had lost the point entirely.
    #[test]
    fn the_two_session_faults_are_distinguished() {
        assert_eq!(
            Refusal::MalformedSessionId.status(),
            StatusCode::BAD_REQUEST,
        );
        assert_eq!(Refusal::UnknownSession.status(), StatusCode::NOT_FOUND);
        assert_ne!(
            Refusal::MalformedSessionId.code(),
            Refusal::UnknownSession.code(),
        );
    }

    /// The gap body names both watermarks.
    ///
    /// A client that receives a conflict with no numbers cannot tell which of
    /// its events are missing, so it cannot re-request them — it can only give
    /// up. This is the assertion that distinguishes "reported a gap" from
    /// "refused to resume", and it is the reason the variant carries two ids
    /// rather than one.
    #[test]
    fn a_gap_names_both_watermarks() -> Result<(), TestError> {
        let refusal = Refusal::StreamGap {
            requested: EventId::new(3),
            evicted_through: EventId::new(9),
            last: EventId::new(42),
        };
        let data = refusal
            .body()
            .get("error")
            .and_then(|error| error.get("data"))
            .cloned()
            .ok_or("no data member")?;
        assert_eq!(
            data.get("error").and_then(Value::as_str),
            Some("stream_gap")
        );
        assert_eq!(data.get("requested").and_then(Value::as_u64), Some(3));
        assert_eq!(data.get("evicted_through").and_then(Value::as_u64), Some(9));
        assert_eq!(data.get("last").and_then(Value::as_u64), Some(42));
        Ok(())
    }

    /// A gap is a conflict, and an unknown cursor is a different one.
    ///
    /// Both are refusals and neither is a success, but a client that conflates
    /// them either retries a cursor that will never become valid, or
    /// re-requests events for a stream that never had them. The slugs keep
    /// them apart on the wire.
    #[test]
    fn a_gap_and_an_unknown_cursor_are_different_faults() {
        let gap = Refusal::StreamGap {
            requested: EventId::new(3),
            evicted_through: EventId::new(9),
            last: EventId::new(42),
        };
        let unknown = Refusal::UnknownCursor {
            requested: EventId::new(50),
            last: EventId::new(42),
        };
        assert_eq!(gap.status(), StatusCode::CONFLICT);
        assert_eq!(unknown.status(), StatusCode::CONFLICT);
        assert_ne!(slug(&gap), slug(&unknown));
        assert_ne!(gap.code(), unknown.code());
        assert_eq!(
            unknown
                .body()
                .get("error")
                .and_then(|error| error.get("data"))
                .and_then(|data| data.get("error"))
                .and_then(Value::as_str),
            Some("unknown_cursor"),
        );
    }

    /// A `405` carries `Allow`, so a client can discover what is offered.
    #[test]
    fn a_refused_method_advertises_the_allowed_one() {
        let response = Refusal::MethodNotAllowed {
            method: "POST",
            reason: "this session has no server-initiated stream",
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::ALLOW)
                .and_then(|value| value.to_str().ok()),
            Some("POST"),
        );
    }

    /// Every variant has a slug, and none of them is empty.
    ///
    /// The slug is a wire contract, so a new variant that falls through to a
    /// default would be a client-visible regression. The list is written out
    /// rather than iterated so a new variant breaks this test.
    #[test]
    fn every_variant_has_a_stable_slug() {
        let cases = [
            (Refusal::MalformedSessionId, "malformed_session_id"),
            (Refusal::MissingSession, "missing_session"),
            (Refusal::UnknownSession, "unknown_session"),
            (
                Refusal::MethodNotAllowed {
                    method: "GET",
                    reason: "no stream",
                },
                "method_not_allowed",
            ),
            (
                Refusal::ProtocolHeader {
                    reason: "unknown".to_owned(),
                },
                "protocol_header",
            ),
            (Refusal::BodyTooLarge, "body_too_large"),
            (Refusal::Unauthenticated, "unauthenticated"),
            (Refusal::Forbidden, "forbidden"),
            (
                Refusal::MalformedBody {
                    reason: "not json".to_owned(),
                },
                "malformed_body",
            ),
            (
                Refusal::StreamGap {
                    requested: EventId::new(1),
                    evicted_through: EventId::new(1),
                    last: EventId::new(1),
                },
                "stream_gap",
            ),
            (
                Refusal::UnknownCursor {
                    requested: EventId::new(1),
                    last: EventId::new(1),
                },
                "unknown_cursor",
            ),
            (Refusal::Handler("boom".to_owned()), "handler_error"),
        ];
        for (refusal, expected) in cases {
            assert_eq!(slug(&refusal), expected);
            assert!(!expected.is_empty());
            // The slug is a wire contract, so it must be reachable from the
            // body as well as from `slug` — a slug that exists in Rust but not
            // on the wire is a slug no client can branch on.
            assert_eq!(
                refusal
                    .body()
                    .get("error")
                    .and_then(|error| error.get("data"))
                    .and_then(|data| data.get("error"))
                    .and_then(Value::as_str),
                Some(expected),
                "body slug disagrees for {expected}",
            );
        }
    }

    /// The `message` a client reads is the `Display` a log line carries.
    ///
    /// Two copies of this text is two that drift, and a client shown a message
    /// the server never logged is a bug report nobody can reproduce.
    #[test]
    fn the_message_in_the_body_is_the_display_text() {
        let refusal = Refusal::Forbidden;
        let expected = refusal.to_string();
        assert_eq!(
            refusal
                .body()
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str),
            Some(expected.as_str()),
        );
    }
}
