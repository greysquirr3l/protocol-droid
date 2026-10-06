//! Wire JSON-RPC envelope to [`IrMessage`] and back, losslessly.
//!
//! The IR has the message types ([`IrRequest`], [`IrResult`], and the rest)
//! but nothing built them. This module is that crossing, and it exists because
//! a bridge cannot work without it: the bridge's whole job is to read a
//! message in one revision's encoding, hand it to the other side in another
//! revision's encoding, and lose nothing the peer sent.
//!
//! ## Why this is not [`Codec`](super::Codec)
//!
//! [`Codec`] handles `ToolResult`, and it is implemented per revision because
//! the revisions genuinely disagree about `CallToolResult`'s shape. The
//! *envelope* is a different animal: `jsonrpc`, `id`, `method`, `params`,
//! `result` and `error` are the same six names in every revision of JSON-RPC
//! the specification has published. Per-revision envelope codecs would be six
//! identical implementations, and the first one to drift would be the bug.
//!
//! ## What "lossless" means here
//!
//! Every member that is not one of those six goes into
//! [`Extensions`], in order, and comes back out. A `2026-07-28` client sending
//! a `_meta` key this build has never heard of, or a top-level field the
//! message model does not name, crosses the bridge intact in both directions.
//!
//! That is what makes the round-trip claim checkable:
//! `lower(lift(v))` re-emits every member of `v`. A test that only inspects
//! the parsed [`IrMessage`] would pass even when the wire form were mangled, so
//! the tests here assert on the re-emitted JSON.
//!
//! ## Number fidelity
//!
//! JSON numbers go through `serde_json::Number`, which keeps an integer from
//! `1.0` distinct from one and does not reformat either. A bridge that turned
//! every number into a float would silently corrupt any peer sending a
//! 64-bit id.

use serde_json::{Map, Value, json};

use super::{
    ExtensionValue, Extensions, IrError, IrErrorCode, IrMessage, IrNotification, IrParams,
    IrRequest, IrResult, JsonObject, MethodName,
};
use crate::domain::RequestId;

/// The envelope members this module models. Anything else is an extension.
const KNOWN: [&str; 6] = ["jsonrpc", "id", "method", "params", "result", "error"];

/// An envelope this build could not lift.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    /// The object is not a JSON-RPC message of any recognised shape.
    ///
    /// That is: no `method` (so not a request or notification) and no
    /// `result`/`error` (so not a reply). JSON-RPC 2.0 calls this
    /// `Invalid Request`, and `-32600` is the right answer for it.
    #[error("the object is not a JSON-RPC request, notification, result or error")]
    NotAMessage,

    /// A `method` that is not a string.
    #[error("the method member is {found}, not a string")]
    MethodNotAString {
        /// The JSON type that was actually there.
        found: &'static str,
    },

    /// An `id` that is neither a string nor a number.
    ///
    /// `null` is *not* an error: it is how a peer reports a request it could
    /// not parse enough to recover an id for, and dropping it would turn a
    /// recoverable client error into an unanswerable one.
    #[error("the id member is {found}, which JSON-RPC does not allow")]
    IdNotAllowed {
        /// The JSON type that was actually there.
        found: &'static str,
    },

    /// An `error` member whose `code` is not an integer.
    #[error("the error code is {found}, not an integer")]
    CodeNotAnInteger {
        /// The JSON type that was actually there.
        found: &'static str,
    },
}

/// Lifts one wire envelope into the IR.
///
/// # Errors
///
/// [`EnvelopeError`] when the object is not a JSON-RPC message, or when a
/// member this module models has the wrong JSON type.
pub fn lift(object: &Map<String, Value>) -> Result<IrMessage, EnvelopeError> {
    let mut extensions = Extensions::empty();
    for (key, value) in object {
        if !KNOWN.contains(&key.as_str()) {
            extensions.insert(key.clone(), ExtensionValue::from(value.clone()));
        }
    }

    let id = match object.get("id") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(RequestId::text(text)),
        // A numeric id stays numeric. Routing it through `RequestId::text`
        // would make a peer that matches replies by id type see a string, and
        // the reply would arrive under an id nothing is waiting for.
        Some(Value::Number(number)) => Some(
            number
                .as_i64()
                .map_or_else(|| RequestId::text(number.to_string()), RequestId::number),
        ),
        Some(other) => {
            return Err(EnvelopeError::IdNotAllowed {
                found: type_name(other),
            });
        }
    };

    if let Some(method) = object.get("method") {
        let name = match method {
            Value::String(text) => text.clone(),
            other => {
                return Err(EnvelopeError::MethodNotAString {
                    found: type_name(other),
                });
            }
        };
        let method = MethodName::new(&name).map_err(|_| EnvelopeError::NotAMessage)?;
        let params = lift_params(object.get("params"));
        // A notification is a request with no id. The distinction is
        // structural in JSON-RPC and the IR keeps it.
        return Ok(match id {
            Some(id) => IrMessage::Request(IrRequest {
                method,
                id: Some(id),
                params,
                extensions,
            }),
            None => IrMessage::Notification(IrNotification {
                method,
                params,
                extensions,
            }),
        });
    }

    if let Some(result) = object.get("result") {
        return Ok(IrMessage::Result(IrResult {
            id: id.ok_or(EnvelopeError::NotAMessage)?,
            params: lift_params(Some(result)),
            extensions,
        }));
    }

    if let Some(error) = object.get("error") {
        return Ok(IrMessage::Error(lift_error(error, id, extensions)?));
    }

    Err(EnvelopeError::NotAMessage)
}

/// Lifts a `params` member, or `result`, into the parameter bag.
///
/// `null` and a missing member are both "no parameters", because JSON-RPC 2.0
/// permits both spellings and the difference is not observable downstream.
fn lift_params(value: Option<&Value>) -> IrParams {
    match value {
        Some(Value::Object(object)) => {
            let mut extensions = Extensions::empty();
            extensions.absorb_json(object);
            IrParams::from_extensions(extensions)
        }
        // An array or a scalar `params` is not something this build models,
        // but it is not the caller's business at this layer either — the
        // handler owns that decision. It arrives as an empty bag rather than
        // being dropped, so a caller can see the shape was not understood.
        _ => IrParams::empty(),
    }
}

fn lift_error(
    value: &Value,
    id: Option<RequestId>,
    extensions: Extensions,
) -> Result<IrError, EnvelopeError> {
    let Value::Object(object) = value else {
        return Err(EnvelopeError::NotAMessage);
    };
    let mut extensions = extensions;
    for (key, member) in object {
        if !matches!(key.as_str(), "code" | "message" | "data") {
            extensions.insert(key.clone(), ExtensionValue::from(member.clone()));
        }
    }

    let code = match object.get("code") {
        Some(Value::Number(number)) => IrErrorCode::from_code(
            number
                .as_i64()
                .ok_or(EnvelopeError::CodeNotAnInteger { found: "a float" })?,
        ),
        Some(other) => {
            return Err(EnvelopeError::CodeNotAnInteger {
                found: type_name(other),
            });
        }
        None => IrErrorCode::InternalError,
    };

    let message = object
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // `IrError::data` is a validated *text* wrapper rather than a parsed map,
    // so the member is re-validated through it rather than trusted: by the
    // time it is here it has already parsed once, so this cannot fail.
    let data = object
        .get("data")
        .map(|value| JsonObject::new(value.to_string().as_str()))
        .transpose()
        .map_err(|_| EnvelopeError::NotAMessage)?;

    Ok(IrError {
        id,
        code,
        message,
        data,
        extensions,
    })
}

/// Re-emits an IR message as a wire envelope.
///
/// The inverse of [`lift`] for everything [`lift`] models: each extension
/// goes back where it came from, so a member the build does not understand
/// still reaches the peer on the other side byte-for-byte as text.
#[must_use]
pub fn lower(message: &IrMessage) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("jsonrpc".to_owned(), json!("2.0"));

    match message {
        IrMessage::Request(request) => {
            object.insert("method".to_owned(), json!(request.method.as_str()));
            set_id(&mut object, request.id.as_ref());
            insert_params(&mut object, &request.params);
            absorb(&mut object, &request.extensions);
        }
        IrMessage::Notification(notification) => {
            object.insert("method".to_owned(), json!(notification.method.as_str()));
            insert_params(&mut object, &notification.params);
            absorb(&mut object, &notification.extensions);
        }
        IrMessage::Result(result) => {
            set_id(&mut object, Some(&result.id));
            object.insert("result".to_owned(), params_to_value(&result.params));
            absorb(&mut object, &result.extensions);
        }
        IrMessage::Error(error) => {
            if let Some(id) = &error.id {
                set_id(&mut object, Some(id));
            } else {
                object.insert("id".to_owned(), Value::Null);
            }
            let mut inner = Map::new();
            inner.insert("code".to_owned(), json!(error.code.code()));
            inner.insert("message".to_owned(), json!(error.message));
            if let Some(data) = &error.data {
                // Parsed back rather than kept as a string: `data` is a JSON
                // *object* on the wire, and re-emitting the validated text as a
                // quoted string would change the message the peer receives.
                inner.insert(
                    "data".to_owned(),
                    serde_json::from_str(data.as_str()).unwrap_or(Value::Null),
                );
            }
            absorb(&mut inner, &error.extensions);
            object.insert("error".to_owned(), Value::Object(inner));
        }
    }
    object
}

fn set_id(object: &mut Map<String, Value>, id: Option<&RequestId>) {
    if let Some(id) = id {
        // `RequestId::to_json` returns the id's *JSON text*, so it is parsed
        // back rather than wrapped. Wrapping would emit `"\"7\""` — a string
        // containing the two characters `"7"` — and a numeric id would arrive
        // at the peer as a string, which is a different id.
        object.insert(
            "id".to_owned(),
            serde_json::from_str(&id.to_json()).unwrap_or(Value::Null),
        );
    }
}

fn insert_params(object: &mut Map<String, Value>, params: &IrParams) {
    if !params.as_extensions().is_empty() {
        object.insert("params".to_owned(), params_to_value(params));
    }
}

fn params_to_value(params: &IrParams) -> Value {
    Value::Object(params.as_extensions().to_json_object())
}

fn absorb(object: &mut Map<String, Value>, extensions: &Extensions) {
    // Goes through `to_json_object` rather than serialising each value: that
    // is the one place that knows how an `ExtensionValue` becomes JSON, so
    // doing it here too would be the second place to learn it.
    for (key, value) in extensions.to_json_object() {
        // An extension never overwrites a modelled member. If a peer sent a
        // member this build models *and* claims the same name in the
        // passthrough bag, the modelled one wins — it is the one we can
        // actually read, and silently replacing it would make the message
        // mean something different than the one we parsed.
        object.entry(key).or_insert(value);
    }
}

const fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}
#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{EnvelopeError, lift, lower};

    type TestError = Box<dyn std::error::Error>;

    fn round_trip(value: Value) -> Result<Value, TestError> {
        let Value::Object(object) = value else {
            return Err("a fixture that is not an object".into());
        };
        let message = lift(&object)?;
        Ok(Value::Object(lower(&message)))
    }

    /// The round trip must reproduce every member of the original.
    ///
    /// This is the test that says the bridge is lossless, and it asserts on
    /// the *re-emitted JSON* rather than the parsed IR on purpose. A test that
    /// only inspected the IR would pass even if lowering mangled the wire form
    /// — which is the failure mode the task calls out by name.
    #[test]
    fn an_unmodelled_directive_survives_the_round_trip() -> Result<(), TestError> {
        let sent = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "echo",
                "arguments": {"text": "hi"},
                "_meta": {"com.example/trace": {"span": 12}}
            },
            "x-vendor-directive": {"anything": [1, 2, 3]},
            "future-top-level": "surprise"
        });

        let back = round_trip(sent.clone())?;

        assert_eq!(back, sent, "the bridge altered the wire form");
        Ok(())
    }

    /// The same claim for a reply, which travels the other direction and has
    /// `result` rather than `params` to carry the extra.
    #[test]
    fn an_unmodelled_result_member_survives_the_round_trip() -> Result<(), TestError> {
        let sent = json!({
            "jsonrpc": "2.0",
            "id": "abc",
            "result": {"content": [], "vendorExtra": {"a": 1}},
            "x-trace": "t-1"
        });

        assert_eq!(round_trip(sent.clone())?, sent);
        Ok(())
    }

    /// Order survives *through the bag*, which is what `Extensions` promises.
    ///
    /// It does not survive into the emitted `serde_json::Map`, and cannot:
    /// this crate builds `serde_json` without `preserve_order`, so a `Map` is
    /// a `BTreeMap` and re-sorts on insertion. JSON object member order is not
    /// semantically significant (RFC 8259 §4), so the wire message is
    /// equivalent — but it is not byte-identical, and a test that claimed
    /// otherwise would be asserting something the build cannot deliver.
    ///
    /// Flipping `preserve_order` on would make the whole claim true and
    /// change `Value` for every consumer, including the JSON-RPC hot path.
    /// That is a decision about the crate, not about the bridge, so it is
    /// raised rather than taken here.
    /// Lowering emits every member it was handed, and drops none.
    ///
    /// Note what this does *not* claim: member order. This crate builds
    /// `serde_json` without `preserve_order`, so a `Map` is a `BTreeMap` and
    /// re-sorts on insertion — the wire order is already gone by the time
    /// `lift` first sees the object, and `lower` cannot put it back.
    ///
    /// `Extensions` still preserves insertion order internally, which is what
    /// its own contract promises; what it cannot promise is that order
    /// survives into a sorted `Map`. Flipping `preserve_order` on would make
    /// order survive end to end, and would change `Value` for every consumer
    /// including the JSON-RPC hot path — a decision about the crate rather
    /// than about the bridge, so it is raised here rather than taken here.
    #[test]
    fn lowering_keeps_every_member() -> Result<(), TestError> {
        let sent = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "ping",
            "zeta": 1,
            "alpha": 2,
            "nested": {"b": [1, 2], "a": null}
        });

        let back = round_trip(sent.clone())?;
        let (Value::Object(forwarded), Value::Object(original)) = (&back, &sent) else {
            return Err("a fixture that is not an object".into());
        };

        assert_eq!(
            forwarded.len(),
            original.len(),
            "lowering dropped or invented a member: {forwarded:?}"
        );
        for (key, value) in original {
            assert_eq!(
                forwarded.get(key),
                Some(value),
                "member {key:?} did not survive"
            );
        }
        Ok(())
    }

    /// A large integer id must not come back as a float. A bridge that routed
    /// every number through `f64` would silently rewrite a peer's id and the
    /// reply would arrive under an id nobody is waiting for.
    #[test]
    fn a_large_integer_id_survives_exactly() -> Result<(), TestError> {
        // 2^53 + 1: the first integer an f64 cannot represent, which is
        // exactly what a float-routed bridge would round away.
        let beyond_float_precision = 9_007_199_254_740_993_i64;
        let sent = json!({
            "jsonrpc": "2.0",
            "id": beyond_float_precision,
            "method": "ping"
        });

        assert_eq!(round_trip(sent.clone())?, sent);
        Ok(())
    }

    /// A notification has no id and must not grow one.
    #[test]
    fn a_notification_stays_a_notification() -> Result<(), TestError> {
        let sent = json!({"jsonrpc": "2.0", "method": "notifications/initialized", "note": 1});

        let back = round_trip(sent.clone())?;
        assert_eq!(back, sent);
        assert!(
            back.get("id").is_none(),
            "a notification gained an id it never had"
        );
        Ok(())
    }

    /// `null` id is how a peer reports a request it could not parse enough to
    /// recover an id for. Dropping it would turn a recoverable client error
    /// into an unanswerable one.
    #[test]
    fn a_null_id_is_kept() -> Result<(), TestError> {
        let sent =
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse"}});

        assert_eq!(round_trip(sent.clone())?, sent);
        Ok(())
    }

    /// An error's `data` is free-form and must survive whole.
    #[test]
    fn error_data_survives() -> Result<(), TestError> {
        let sent = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "error": {"code": -32602, "message": "bad", "data": {"detail": [1, {"x": null}]}}
        });

        assert_eq!(round_trip(sent.clone())?, sent);
        Ok(())
    }

    /// The failure cases, each naming its own variant rather than letting a
    /// test helper collapse them into one "did not work".
    #[test]
    fn malformed_envelopes_are_refused_by_kind() -> Result<(), TestError> {
        let cases: [(&str, Value); 3] = [
            ("nothing", json!({"jsonrpc": "2.0"})),
            (
                "method type",
                json!({"jsonrpc": "2.0", "id": 1, "method": 5}),
            ),
            (
                "id type",
                json!({"jsonrpc": "2.0", "id": [], "method": "ping"}),
            ),
        ];

        for (label, value) in cases {
            let Value::Object(object) = &value else {
                return Err("a fixture that is not an object".into());
            };
            let Err(refusal) = lift(object) else {
                return Err(format!("{label} should have been refused").into());
            };
            match (label, refusal) {
                ("nothing", EnvelopeError::NotAMessage) => {}
                ("method type", EnvelopeError::MethodNotAString { found }) => {
                    assert_eq!(found, "a number");
                }
                ("id type", EnvelopeError::IdNotAllowed { found }) => {
                    assert_eq!(found, "an array");
                }
                _ => return Err(format!("{label} reported the wrong refusal").into()),
            }
        }
        Ok(())
    }
}
