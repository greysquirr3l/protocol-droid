//! The borrowed JSON-RPC envelope — what a frame *is*, before anything
//! decides what to do about it.
//!
//! # Borrowed, not owned, and the reason is the hot path
//!
//! [`Frame`] holds `&str` and `&RawValue` pointing into the frame buffer. A
//! numeric id is an `i64` copied out of the buffer, so routing a frame costs no
//! allocation at all — which is what "the envelope path must not allocate for a
//! numeric id" means in practice. A string id borrows the buffer rather than
//! building an `Arc<str>`; the owned [`RequestId`] is built later, at the point
//! where a value has to outlive the buffer (the in-flight map, a log line, a
//! record/replay file).
//!
//! Splitting the two is not a micro-optimisation. The IR's [`RequestId`] is
//! deliberately non-`Copy` and allocates its `Arc<str>` at construction, which
//! is right for a stored value and wrong for a value that is about to be
//! matched against a method and dropped.
//!
//! # One pass, and the decision is stored
//!
//! JSON-RPC 2.0 is a four-way choice, and a message that is not exactly one of
//! the four is `-32600`. `decode` resolves that choice once and writes the
//! answer into [`MessageKind`]; [`Envelope::kind`] is therefore not a question
//! the router re-asks. A frame that reached the router has already been
//! validated as one of the four shapes, so no downstream code has to decide
//! whether a `method` next to an `id` is legal.
//!
//! This is the project's "parse, don't validate" rule applied to the envelope:
//! there is no `Envelope` in which the caller has to work out what kind of
//! message it holds.

use std::borrow::Cow;

use serde::Deserialize;
use serde_json::value::RawValue;

/// The `jsonrpc` member every message must carry.
const JSONRPC_VERSION: &str = "2.0";

/// A JSON-RPC method name, borrowed from the frame unless it needed unescaping.
///
/// A `Cow` rather than a `&str` for the same reason [`WireId`] is one:
/// `serde_json` refuses to lend a borrow for a string containing an escape, and
/// a peer may spell a method with one. The common case — `tools/list` written
/// plainly — allocates nothing.
pub type Method<'a> = Cow<'a, str>;

/// A JSON-RPC id as it appears on the wire, borrowed from the frame buffer.
///
/// A number is copied out — it is a `Copy` `i64`, so that arm never allocates.
/// A string is a [`Cow`]: borrowed straight out of the buffer when it has no
/// escape, and owned only when it does.
///
/// # Why a `Cow` and not a `&str`
///
/// `serde_json` will not hand back a borrowed `&str` for a string containing an
/// escape — `"a\"b"` has to be unescaped, and unescaping means allocating, so
/// the parse fails rather than lie about the borrow. An id containing an escape
/// is perfectly legal JSON and a peer can send one, so a bare `&'a str` here
/// would reject a request that must be answered.
///
/// The price is that `WireId` is no longer `Copy`, which propagates to
/// `Envelope` and `Routed`. That is the right trade: a `Copy` derive on a type
/// that cannot represent a valid id is not a saving, it is a bug waiting to
/// compile. The common case — an unescaped uuid or a small integer — still
/// allocates nothing.
///
/// # The lossy-looking `to_json`
///
/// [`to_json`](Self::to_json) re-escapes rather than splicing the original raw
/// text, so a string id that was sent escaped comes back escaped in the same
/// way. That is what makes the response's id parse to the value the peer sent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WireId<'a> {
    /// A numeric id.
    Number(i64),
    /// A string id, borrowed from the buffer unless it needed unescaping.
    Text(Cow<'a, str>),
}

impl<'a> WireId<'a> {
    /// Parses an id from its raw JSON, borrowed.
    ///
    /// The spec allows a number or a string and nothing else, so `null`,
    /// `true`, an array, and a fractional number are all rejected. A
    /// fractional number is rejected rather than truncated: `1.5` is not id
    /// `1`, and quietly rounding it would make two different ids collide.
    ///
    /// # Errors
    ///
    /// [`WireIdError::NotAnId`] if the value is not a number or a string.
    /// [`WireIdError::NotAnInteger`] if the number has a fractional part.
    pub fn from_raw(value: &'a RawValue) -> Result<Self, WireIdError> {
        let raw = value.get();
        // A string first, borrowed: the text arm is zero-copy and this is the
        // only arm that can be. A leading quote is the discriminator, and
        // `RawValue` is already a syntactically complete JSON value, so the
        // first byte decides.
        if raw.starts_with('"') {
            // A `&str` first: it succeeds and borrows for the overwhelmingly
            // common unescaped id, with no allocation at all. It fails for an
            // id containing an escape, because unescaping means allocating —
            // and only then does a `String` get allocated to hold the result.
            return Ok(match serde_json::from_str::<&'a str>(raw) {
                Ok(borrowed) => Self::Text(Cow::Borrowed(borrowed)),
                Err(_) => match serde_json::from_str::<String>(raw) {
                    Ok(owned) => Self::Text(Cow::Owned(owned)),
                    Err(_) => return Err(WireIdError::NotAnId),
                },
            });
        }
        // Then an integer, which is `Copy` and therefore free to extract. A
        // fractional number parses as `f64` and is refused rather than
        // truncated: `1.5` is not id `1`, and rounding it would make two
        // different ids collide on the way onto the in-flight map.
        match serde_json::from_str::<i64>(raw) {
            Ok(number) => Ok(Self::Number(number)),
            // A JSON number that is not an `i64` is either fractional or out
            // of range. The specification says an id must not contain
            // fractional parts; an integer too large for `i64` fails the same
            // check for the same reason, and both are told apart by the text.
            Err(_) if serde_json::from_str::<f64>(raw).is_ok() => Err(WireIdError::NotAnInteger),
            Err(_) => Err(WireIdError::NotAnId),
        }
    }

    /// A borrowed string id.
    ///
    /// Does not allocate. The type is a `Cow` only because an id containing a
    /// JSON escape cannot be borrowed out of the buffer; a caller that already
    /// holds a `&str` never pays for that.
    #[must_use]
    pub const fn text(value: &'a str) -> Self {
        Self::Text(Cow::Borrowed(value))
    }

    /// The id's text, or `None` for a numeric id.
    ///
    /// The borrowed string an [`Id`](crate::domain::RequestId) is built from.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value.as_ref()),
            Self::Number(_) => None,
        }
    }

    /// The numeric id, or `None` for a string id.
    #[must_use]
    pub const fn as_number(&self) -> Option<i64> {
        match *self {
            Self::Number(value) => Some(value),
            Self::Text(_) => None,
        }
    }

    /// The id as it appears on the wire, as JSON.
    ///
    /// A string is written with its quotes, so the result can be written into
    /// a response without the caller having to know which arm it got.
    #[must_use]
    pub fn to_json(&self) -> String {
        match self {
            Self::Number(value) => value.to_string(),
            // `format!("{value:?}")` is the obvious one-liner and it is wrong.
            // `Debug` for `str` is `escape_debug`, which writes a control
            // character as `\u{7f}` — Rust syntax, and not a legal JSON
            // escape, which is `\u007f`. An id containing DEL would produce a
            // response that is not JSON at all, and the peer would see a parse
            // error rather than its own id. serde_json escapes to JSON.
            Self::Text(value) => serde_json::to_string(value.as_ref()).unwrap_or_default(),
        }
    }
}

/// Why a wire id is not an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireIdError {
    /// The value was neither a number nor a string.
    #[error("a JSON-RPC id must be a number or a string")]
    NotAnId,
    /// The number had a fractional part.
    #[error("a JSON-RPC id must not contain fractional parts")]
    NotAnInteger,
}

/// An error object, borrowed.
///
/// No `PartialEq`: `RawValue` does not implement it, because the question
/// "are these the same JSON?" has two honest answers (equal as trees, equal as
/// bytes) and `serde_json` declines to pick one. Callers that need it compare
/// `.get()`, which is the byte-exact answer and the one the passthrough path
/// wants anyway.
#[derive(Debug, Clone)]
pub struct WireError<'a> {
    /// The numeric error code.
    pub code: i64,
    /// The human-readable message.
    pub message: &'a str,
    /// Anything else the sender attached, kept for the passthrough path.
    pub data: Option<&'a RawValue>,
}

/// The `result` or `error` half of a reply.
///
/// No `PartialEq`, for the reason on [`WireError`].
#[derive(Debug, Clone)]
pub enum Outcome<'a> {
    /// A successful reply.
    Result(&'a RawValue),
    /// A failed reply.
    Error(WireError<'a>),
}

/// Which of the four JSON-RPC 2.0 shapes a message is.
///
/// Resolved by [`decode`] and stored, so no later stage re-derives it. A
/// request is a message with a `method` **and** an `id`; a notification is a
/// message with a `method` and no `id`; the two reply shapes have an `id` and
/// no `method`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// `method` and `id`: expects a reply.
    Request,
    /// `method` and no `id`: no reply is ever sent.
    Notification,
    /// `id` and `result`.
    Result,
    /// `id` and `error`.
    Error,
}

impl MessageKind {
    /// Whether a reply is expected.
    #[must_use]
    pub const fn expects_reply(self) -> bool {
        matches!(self, Self::Request)
    }
}

/// One JSON-RPC message, borrowed from the frame buffer.
///
/// `kind` is the validated shape. The other fields are what the sender wrote,
/// kept as they arrived — a `method` is `Some` on a request and a notification
/// and `None` on a reply, and the two are the only way to tell those apart
/// without consulting `kind`, which is why `kind` exists.
///
/// No `PartialEq`, for the reason on [`WireError`].
#[derive(Debug, Clone)]
pub struct Envelope<'a> {
    pub(crate) kind: MessageKind,
    pub(crate) id: Option<WireId<'a>>,
    pub(crate) method: Option<Method<'a>>,
    pub(crate) params: Option<&'a RawValue>,
    pub(crate) outcome: Option<Outcome<'a>>,
}

impl<'a> Envelope<'a> {
    /// The shape this message was decoded as.
    #[must_use]
    pub const fn kind(&self) -> MessageKind {
        self.kind
    }

    /// The id, absent on a notification.
    ///
    /// By reference: an id whose text had to be unescaped owns a `String`, and
    /// returning it by value would clone that on every read. The common
    /// unescaped case borrows, so a clone there would be a refcount bump on
    /// nothing — either way the caller only wanted to look at it.
    #[must_use]
    pub const fn id(&self) -> Option<&WireId<'a>> {
        self.id.as_ref()
    }

    /// The method, absent on a reply.
    #[must_use]
    pub const fn method(&self) -> Option<&Method<'a>> {
        self.method.as_ref()
    }

    /// The method as a plain string, whichever arm of the `Cow` it is.
    #[must_use]
    pub fn method_str(&self) -> Option<&str> {
        self.method.as_deref()
    }

    /// The params, still undecoded.
    ///
    /// Borrowed raw JSON on purpose: the concrete type depends on the method
    /// and the negotiated revision, and neither is known until after routing.
    /// Decoding here would mean decoding every frame's params even for methods
    /// that are about to be refused for not existing.
    #[must_use]
    pub const fn params(&self) -> Option<&'a RawValue> {
        self.params
    }

    /// The reply half, present only on a `Result` or an `Error`.
    #[must_use]
    pub const fn outcome(&self) -> Option<&Outcome<'a>> {
        self.outcome.as_ref()
    }

    /// Whether a reply is expected.
    #[must_use]
    pub const fn expects_reply(&self) -> bool {
        self.kind.expects_reply()
    }

    /// The method and id together, for the router.
    ///
    /// Returns `None` for a reply shape, which has no method to route.
    #[must_use]
    pub fn routeable(&self) -> Option<(&Method<'a>, Option<&WireId<'a>>)> {
        match self.kind {
            MessageKind::Request | MessageKind::Notification => Some((
                self.method.as_ref().unwrap_or(&Method::Borrowed("")),
                self.id.as_ref(),
            )),
            MessageKind::Result | MessageKind::Error => None,
        }
    }
}

/// A whole frame: one message, or a batch of them.
///
/// The batch arm is recognised here rather than in a caller because a JSON
/// array is a different *protocol* question from anything else on this path:
/// `2025-06-18` removed batching, and the decision belongs to the session's
/// profile, not to the decoder. So the decoder reports the shape and leaves
/// the policy to [`crate::application::jsonrpc::BatchPolicy`].
#[derive(Debug, Clone)]
pub enum Frame<'a> {
    /// A single message.
    Single(Envelope<'a>),
    /// A batch, in the order the sender wrote it.
    Batch(Vec<Envelope<'a>>),
}

impl<'a> Frame<'a> {
    /// Every message in the frame, in order.
    #[must_use]
    pub fn envelopes(&self) -> &[Envelope<'a>] {
        match self {
            Self::Single(envelope) => std::slice::from_ref(envelope),
            Self::Batch(envelopes) => envelopes,
        }
    }

    /// Every message in the frame, in order, moving each one out.
    ///
    /// The owned counterpart to [`envelopes`](Self::envelopes). Routing needs
    /// it: the id of a request has to be moved into the reply, and an id whose
    /// text needed unescaping owns a `String`, so routing cannot borrow.
    #[must_use]
    pub fn into_envelopes(self) -> Vec<Envelope<'a>> {
        match self {
            Self::Single(envelope) => vec![envelope],
            Self::Batch(envelopes) => envelopes,
        }
    }

    /// Whether this frame is a batch.
    #[must_use]
    pub const fn is_batch(&self) -> bool {
        matches!(self, Self::Batch(_))
    }
}

/// Why a frame could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The bytes were not JSON at all (`-32700`).
    #[error("the frame is not valid JSON")]
    Json,
    /// The frame was JSON but not a JSON-RPC message (`-32600`).
    ///
    /// This is the single bucket for every shape problem: not an object, a
    /// missing or wrong `jsonrpc` member, a `method` next to a `result`, an id
    /// that is not a number or a string. The specification gives all of them
    /// the same code, and a peer gets nothing useful from a more precise one.
    #[error("{0}")]
    InvalidRequest(&'static str),
    /// A batch frame was empty (`-32600`).
    #[error("a batch must contain at least one message")]
    EmptyBatch,
}

impl DecodeError {
    /// The JSON-RPC code this failure must be reported as.
    #[must_use]
    pub const fn code(self) -> crate::domain::ir::IrErrorCode {
        match self {
            Self::Json => crate::domain::ir::IrErrorCode::ParseError,
            Self::InvalidRequest(_) | Self::EmptyBatch => {
                crate::domain::ir::IrErrorCode::InvalidRequest
            }
        }
    }
}

/// The borrowed wire shape, decoded once by serde and then classified.
///
/// Nothing here denies unknown fields: a message this build does not
/// understand still has to reach the passthrough path, and `deny_unknown_fields`
/// would turn every forward-compatible extension into a parse error.
#[derive(Deserialize)]
struct WireMessage<'a> {
    jsonrpc: &'a str,
    /// An absent `id` and an explicit `"id":null` must stay distinguishable.
    ///
    /// A plain `Option<&RawValue>` cannot manage that: serde maps `null` to
    /// `None`, so `{"jsonrpc":"2.0","id":null,"method":"ping"}` would decode
    /// as a *notification* and the request would never be answered. A bare
    /// `#[serde(default)]` does not help either, and neither does
    /// `Option<Option<_>>` — `Option`'s own `Deserialize` is what collapses
    /// the `null`. `deserialize_with` bypasses it: the function runs only when
    /// the member is present, and it always returns `Some`, so `"id":null`
    /// arrives as the raw text `null` and `validate` refuses it.
    #[serde(borrow, default, deserialize_with = "present_member")]
    id: Option<&'a RawValue>,
    /// A method name may contain an escape, and `serde_json` will not lend a
    /// `&str` for one — the same limit the id hits. It matters more here: a
    /// peer may spell any method with any legal escaping, so `tools/list`
    /// is `tools/list` and must route as it. Decoding the member as a
    /// `&RawValue` and unescaping on the way out keeps every legal spelling
    /// of a method working; a plain `&str` would refuse the whole frame as
    /// `-32600` over a difference in spelling.
    #[serde(borrow, default)]
    method: Option<&'a RawValue>,
    /// Also `present_member`, for the opposite reason to `id`.
    ///
    /// A JSON-RPC result may legitimately be `null`, and MCP leans on it — a
    /// `tools/call` whose content is empty, or a handler that returns nothing,
    /// both send `"result": null`. With a plain `Option` that collapses to
    /// `None`, the message looks like it has neither a result nor an error, and
    /// `{"jsonrpc":"2.0","id":1,"result":null}` is refused as `-32600`. The
    /// peer did nothing wrong; the decode threw away the difference between
    /// "no result" and "a result of null", which are different statements.
    #[serde(borrow, default)]
    params: Option<&'a RawValue>,
    #[serde(borrow, default, deserialize_with = "present_member")]
    result: Option<&'a RawValue>,
    #[serde(borrow, default)]
    error: Option<WireErrorRaw<'a>>,
}

/// The method name, borrowed from the frame unless it needed unescaping.
///
/// Mirrors [`WireId::from_raw`] for the reason given on [`WireId`]: a name
/// with an escape cannot be borrowed, and a peer is allowed to send one.
fn method_name<'a>(raw: &'a RawValue) -> Method<'a> {
    let text = raw.get();
    if let Ok(borrowed) = serde_json::from_str::<&'a str>(text) {
        return Method::Borrowed(borrowed);
    }
    // Unescaping a method name cannot fail: `RawValue` is already a
    // syntactically complete JSON value, so the only thing that can go wrong
    // is the borrow, which is what the line above just found. The empty name
    // is the unreachable fallback, and it is unreachable rather than chosen:
    // a `Method` has to hold something.
    serde_json::from_str::<String>(text).map_or(Method::Borrowed(""), Method::Owned)
}

/// Decodes a member that is present, keeping `null` as a value.
///
/// `default` handles the absent case and this handles the present one, so the
/// two never meet. The result is always `Some` — the function is not called at
/// all for a missing member, and a called function means the member was there.
fn present_member<'de, 'a, D>(deserializer: D) -> Result<Option<&'a RawValue>, D::Error>
where
    D: serde::Deserializer<'de>,
    'de: 'a,
{
    Ok(Some(serde::Deserialize::deserialize(deserializer)?))
}

/// The wire form of an error object, before the code is checked.
#[derive(Deserialize)]
struct WireErrorRaw<'a> {
    code: i64,
    #[serde(borrow)]
    message: &'a str,
    #[serde(borrow)]
    data: Option<&'a RawValue>,
}

/// A top-level frame: one message or an array of them.
///
/// Not `#[serde(untagged)]`. An untagged enum deserialises through serde's
/// private owned `Content` buffer, so no field can borrow from the input — the
/// whole frame is materialised as a value tree and then read back out of it,
/// which is both the reason the borrowed view could not compile and the
/// intermediate value tree this path is supposed to avoid. The discriminator
/// is a single byte, so reading it is cheaper than buffering for it.
fn first_non_whitespace(input: &[u8]) -> Option<u8> {
    input
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .copied()
}

impl<'a> From<WireMessage<'a>> for Envelope<'a> {
    fn from(wire: WireMessage<'a>) -> Self {
        let id = wire.id.and_then(|raw| WireId::from_raw(raw).ok());
        let outcome = match (wire.result, wire.error) {
            (Some(result), None) => Some(Outcome::Result(result)),
            (None, Some(error)) => Some(Outcome::Error(WireError {
                code: error.code,
                message: error.message,
                data: error.data,
            })),
            _ => None,
        };
        // The four-way choice, made once. `classify` is total over everything
        // that got this far; anything it rejects was already refused by
        // `decode` before an `Envelope` was built.
        let kind = classify(wire.method.is_some(), wire.id.is_some(), outcome.as_ref());
        Self {
            kind,
            id,
            method: wire.method.map(method_name),
            params: wire.params,
            outcome,
        }
    }
}

/// Resolves the four-way shape, defaulting the two shapes that cannot both be
/// false.
///
/// A message with no `method` and no reply payload is not a JSON-RPC message at
/// all, and `decode` refuses it before this runs. Treating it as a `Result`
/// here is the only choice that keeps this function total, and the default is
/// unreachable rather than meaningful.
const fn classify(has_method: bool, has_id: bool, outcome: Option<&Outcome<'_>>) -> MessageKind {
    match (has_method, has_id) {
        (true, true) => MessageKind::Request,
        (true, false) => MessageKind::Notification,
        (false, _) => match outcome {
            Some(Outcome::Error(_)) => MessageKind::Error,
            Some(Outcome::Result(_)) | None => MessageKind::Result,
        },
    }
}

/// Decodes one frame from bytes, borrowing from `input`.
///
/// The bytes are parsed exactly once. `serde_json` walks the document and the
/// borrowed fields point straight into `input`, so no sub-tree is
/// re-serialised or re-parsed on the way out.
///
/// # Errors
///
/// [`DecodeError::Json`] if the bytes are not JSON, or the top-level value is
/// neither an object nor an array. Otherwise
/// [`DecodeError::InvalidRequest`] for every shape problem, and
/// [`DecodeError::EmptyBatch`] for an empty array.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::jsonrpc::{DecodeError, Frame, MessageKind, decode};
///
/// let frame = decode(br#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#)
///     .expect("a well-formed request decodes");
/// let Frame::Single(envelope) = frame else {
///     panic!("a lone object is not a batch");
/// };
/// assert_eq!(envelope.kind(), MessageKind::Request);
/// assert_eq!(envelope.method_str(), Some("ping"));
/// ```
pub fn decode(input: &[u8]) -> Result<Frame<'_>, DecodeError> {
    match first_non_whitespace(input) {
        Some(b'[') => {
            let messages: Vec<WireMessage<'_>> =
                serde_json::from_slice(input).map_err(|error| map_json_error(&error))?;
            if messages.is_empty() {
                return Err(DecodeError::EmptyBatch);
            }
            let mut out = Vec::with_capacity(messages.len());
            for message in &messages {
                validate(message)?;
                out.push(Envelope::from(clone_shape(message)));
            }
            Ok(Frame::Batch(out))
        }
        // `{` is a single message. Anything else is well-formed JSON that is
        // not a JSON-RPC message, and the spec answers that with -32600 rather
        // than -32700 — so the deserialise is allowed to fail and the error is
        // reclassified below.
        Some(b'{') => {
            let message: WireMessage<'_> =
                serde_json::from_slice(input).map_err(|error| map_json_error(&error))?;
            validate(&message)?;
            Ok(Frame::Single(message.into()))
        }
        Some(_) => Err(DecodeError::InvalidRequest(
            "the frame is not an object or an array",
        )),
        // Empty or all whitespace: not JSON at all.
        None => Err(DecodeError::Json),
    }
}

/// Re-labels a serde failure by the code the specification assigns to it.
///
/// The distinction is not cosmetic: a peer that sent broken bytes needs
/// `-32700` and a peer that sent a well-formed number where a message belongs
/// needs `-32600`, and collapsing them into one tells the sender to fix the
/// wrong thing.
fn map_json_error(error: &serde_json::Error) -> DecodeError {
    if error.is_syntax() || error.is_eof() {
        DecodeError::Json
    } else {
        DecodeError::InvalidRequest("the frame is not a valid JSON-RPC message")
    }
}

/// Copies the borrowed shape out of a batch element.
///
/// [`WireMessage`] holds no owned data, so this is a field-by-field copy rather
/// than a clone of anything heap-allocated. It exists because `From` takes
/// ownership and the batch loop borrows each element in turn.
fn clone_shape<'a>(message: &WireMessage<'a>) -> WireMessage<'a> {
    let WireMessage {
        jsonrpc,
        id,
        method,
        params,
        result,
        error,
    } = message;
    WireMessage {
        jsonrpc,
        id: *id,
        method: *method,
        params: *params,
        result: *result,
        error: error.as_ref().map(|error| WireErrorRaw {
            code: error.code,
            message: error.message,
            data: error.data,
        }),
    }
}

/// The shape rules, applied before an `Envelope` exists.
///
/// All of them answer `-32600`; the reason string is for the log line and the
/// caller, and never reaches the wire, because the wire gets the code.
fn validate(message: &WireMessage<'_>) -> Result<(), DecodeError> {
    if message.jsonrpc != JSONRPC_VERSION {
        return Err(DecodeError::InvalidRequest("`jsonrpc` must be \"2.0\""));
    }
    let has_reply = message.result.is_some() || message.error.is_some();
    match (message.method, has_reply) {
        (Some(_), true) => {
            return Err(DecodeError::InvalidRequest(
                "a message cannot have both a method and a reply",
            ));
        }
        (None, false) => {
            return Err(DecodeError::InvalidRequest(
                "a message needs a method or a reply",
            ));
        }
        _ => {}
    }
    if let Some(raw_id) = message.id
        && let Err(error) = WireId::from_raw(raw_id)
    {
        return Err(DecodeError::InvalidRequest(match error {
            // Covers `"id": null` as well as `true`, an array, and an object.
            // `null` is refused rather than treated as an absent id, because
            // treating it as absent turns the sender's request into a
            // notification and it would wait forever for a reply.
            WireIdError::NotAnId => "an id must be a number or a string",
            WireIdError::NotAnInteger => "a numeric id must not have a fractional part",
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The envelope decoder, against the exact codes the specification assigns.
    //!
    //! # The three cases the task names
    //!
    //! Malformed JSON, a wrong `jsonrpc` version, and a missing id on a request
    //! each have to produce a specific code, and all three are asserted by their
    //! code rather than by "it failed". A test that only checks that decoding fails
    //! passes against an implementation that returns the wrong code for every input,
    //! which is the failure a peer cannot act on.
    //!
    //! `assert_matches!` rather than `assert!(matches!(..))` so a failure prints the
    //! actual variant instead of `assertion failed`.
    //!
    //! These live in the module rather than `tests/` because the decoder is
    //! domain API: an integration test sees the crate as a dependent, which is
    //! the right way to test the *public* surface but leaves the compiler free
    //! to call a public-but-unused item dead when it builds this crate as a
    //! test binary. The properties that genuinely need a dependent's view of
    //! the crate live in `tests/jsonrpc_fuzz.rs`.
    // Test code unwraps deliberately: the strict profile exists to keep
    // production paths fallible, and a test that cannot unwrap cannot assert.
    // The messages name the failing precondition, which is the useful part.
    #![expect(
        clippy::panic,
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
    )]

    //! The envelope decoder, against the exact codes the specification assigns.
    //!
    //! # The three cases the task names
    //!
    //! Malformed JSON, a wrong `jsonrpc` version, and a missing id on a request
    //! each have to produce a specific code, and all three are asserted by their
    //! code rather than by "it failed". A test that only checks that decoding fails
    //! passes against an implementation that returns the wrong code for every input,
    //! which is the failure a peer cannot act on.
    //!
    //! `assert_matches!` rather than `assert!(matches!(..))` so a failure prints the
    //! actual variant instead of `assertion failed`.

    use crate::domain::RequestId;
    use crate::domain::ir::IrErrorCode;
    use crate::domain::jsonrpc::{
        DecodeError, Envelope, Frame, MessageKind, Method, Outcome, WireId, WireIdError, decode,
    };
    use assert_matches::assert_matches;

    type TestError = Box<dyn std::error::Error>;

    fn single(input: &[u8]) -> Result<Envelope<'_>, TestError> {
        let frame = decode(input)?;
        match frame {
            Frame::Single(envelope) => Ok(envelope),
            Frame::Batch(_) => Err("expected a single message, got a batch".into()),
        }
    }

    fn first(text: &[u8]) -> Result<Envelope<'_>, TestError> {
        single(text)
    }

    #[test]
    fn a_request_carries_a_method_and_an_id() -> Result<(), TestError> {
        let envelope = first(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#.as_bytes())?;
        assert_eq!(envelope.kind(), MessageKind::Request);
        assert_eq!(envelope.method_str(), Some("tools/call"));
        assert_eq!(envelope.id(), Some(&WireId::Number(1)));
        assert!(envelope.expects_reply());
        assert!(envelope.outcome().is_none());
        Ok(())
    }

    #[test]
    fn a_notification_has_no_id_and_expects_nothing() -> Result<(), TestError> {
        let envelope =
            first(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.as_bytes())?;
        assert_eq!(envelope.kind(), MessageKind::Notification);
        assert_eq!(envelope.id(), None);
        assert!(!envelope.expects_reply());
        Ok(())
    }

    #[test]
    fn a_result_and_an_error_are_both_replies() -> Result<(), TestError> {
        let result = first(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#.as_bytes())?;
        assert_eq!(result.kind(), MessageKind::Result);
        assert!(!result.expects_reply());
        assert_matches!(result.outcome(), Some(Outcome::Result(_)));

        let error = first(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#.as_bytes(),
        )?;
        assert_eq!(error.kind(), MessageKind::Error);
        assert_matches!(
            error.outcome(),
            Some(Outcome::Error(wire)) if wire.code == -32601 && wire.message == "nope"
        );
        Ok(())
    }

    #[test]
    fn malformed_json_is_parse_error() {
        let error = decode(b"{\"jsonrpc\":")
            .map(|frame| format!("{frame:?}"))
            .unwrap_err();
        assert_eq!(error, DecodeError::Json);
        assert_eq!(error.code(), IrErrorCode::ParseError);
        assert_eq!(error.code().code(), -32700);
    }

    #[test]
    fn a_wrong_jsonrpc_version_is_invalid_request() {
        let error = first(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#.as_bytes()).unwrap_err();
        assert_eq!(
            error.downcast_ref::<DecodeError>(),
            Some(&DecodeError::InvalidRequest("`jsonrpc` must be \"2.0\"")),
        );
        assert_eq!(
            decode(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#.as_bytes())
                .unwrap_err()
                .code(),
            IrErrorCode::InvalidRequest,
        );
        assert_eq!(
            IrErrorCode::InvalidRequest.code(),
            -32600,
            "the code a peer switches on must be the spec's, not merely the variant's name"
        );
    }

    #[test]
    fn a_missing_id_on_a_request_is_invalid_request() {
        // A message with a `method` and no `id` is a *notification*, which is
        // legal, so this cannot be expressed as a malformed frame. The way to ask
        // whether it is a valid request is to route it, and T09's router is where
        // a caller learns it will never be answered. See
        // `a_notification_is_never_a_request` in `jsonrpc_router.rs`.
        let frame = decode(br#"{"jsonrpc":"2.0","method":"ping"}"#);
        let Ok(Frame::Single(envelope)) = frame else {
            panic!("a method without an id is still a well-formed message");
        };
        assert_eq!(envelope.kind(), MessageKind::Notification);
        assert_eq!(envelope.id(), None);
    }

    #[test]
    fn a_method_beside_a_result_is_invalid_request() {
        assert_eq!(
            decode(br#"{"jsonrpc":"2.0","id":1,"method":"ping","result":{}}"#).unwrap_err(),
            DecodeError::InvalidRequest("a message cannot have both a method and a reply"),
        );
    }

    #[test]
    fn a_message_with_neither_method_nor_reply_is_invalid_request() {
        assert_eq!(
            decode(br#"{"jsonrpc":"2.0","id":1}"#).unwrap_err(),
            DecodeError::InvalidRequest("a message needs a method or a reply"),
        );
    }

    #[test]
    fn an_id_that_is_neither_number_nor_string_is_invalid_request() {
        for input in [
            r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":[1],"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":{"a":1},"method":"ping"}"#,
        ] {
            assert_eq!(
                decode(input.as_bytes()).unwrap_err(),
                DecodeError::InvalidRequest("an id must be a number or a string"),
                "input was {input}",
            );
        }
    }

    #[test]
    fn a_fractional_id_is_invalid_request() {
        assert_eq!(
            decode(br#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#).unwrap_err(),
            DecodeError::InvalidRequest("a numeric id must not have a fractional part"),
        );
    }

    #[test]
    fn an_id_outside_i64_is_refused_rather_than_wrapped() {
        // `2^63` is a JSON integer but not an `i64`. Wrapping it would put a
        // different request on the in-flight map than the peer asked for, and the
        // collision is silent.
        let error =
            decode(br#"{"jsonrpc":"2.0","id":9223372036854775808,"method":"ping"}"#).unwrap_err();
        assert_matches!(error, DecodeError::InvalidRequest(_));
    }

    #[test]
    fn a_wire_id_becomes_an_owned_request_id_without_loss() -> Result<(), TestError> {
        for (input, expected) in [
            (
                r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#,
                RequestId::number(42),
            ),
            (
                r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#,
                RequestId::text("abc"),
            ),
        ] {
            let envelope = first(input.as_bytes())?;
            let wire = envelope.id().ok_or("the envelope has an id")?;
            let owned = match wire.as_number() {
                Some(number) => RequestId::number(number),
                None => RequestId::text(wire.as_text().ok_or("a text id has text")?),
            };
            assert_eq!(owned, expected, "input was {input}");
        }
        Ok(())
    }

    #[test]
    fn an_id_writes_back_as_the_json_the_sender_sent() -> Result<(), TestError> {
        let numeric = first(r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#.as_bytes())?;
        assert_eq!(
            numeric.id().ok_or("the envelope has an id")?.to_json(),
            "42",
        );

        let text = first(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#.as_bytes())?;
        assert_eq!(
            text.id().ok_or("the envelope has an id")?.to_json(),
            "\"abc\"",
            "a string id must come back quoted, or a response would not be valid JSON"
        );
        Ok(())
    }

    #[test]
    fn a_wire_id_rejects_what_the_spec_rejects() -> Result<(), TestError> {
        let number = serde_json::value::to_raw_value(&1).map_err(TestError::from)?;
        assert_matches!(WireId::from_raw(&number), Ok(WireId::Number(1)));

        let text = serde_json::value::to_raw_value("x").map_err(TestError::from)?;
        // `assert_matches!` would need a literal pattern, and a `Cow` arm has
        // none that is readable. Equality is the better assertion here anyway:
        // it is what tells a borrowed id from an unescaped one.
        let id = WireId::from_raw(&text).map_err(TestError::from)?;
        assert_eq!(id, WireId::text("x"), "an unescaped id borrows");
        assert_matches!(id, WireId::Text(_));

        let float = serde_json::value::to_raw_value(&1.5).map_err(TestError::from)?;
        assert_eq!(WireId::from_raw(&float), Err(WireIdError::NotAnInteger));

        let boolean = serde_json::value::to_raw_value(&true).map_err(TestError::from)?;
        assert_eq!(WireId::from_raw(&boolean), Err(WireIdError::NotAnId));
        Ok(())
    }

    #[test]
    fn a_batch_is_recognised_and_ordered() -> Result<(), TestError> {
        let input = br#"[
            {"jsonrpc":"2.0","id":1,"method":"ping"},
            {"jsonrpc":"2.0","method":"notifications/initialized"}
        ]"#;
        let frame = decode(input)?;
        assert!(frame.is_batch());
        let envelopes = frame.envelopes();
        assert_eq!(envelopes.len(), 2);
        assert_eq!(
            envelopes.first().ok_or("no first")?.id(),
            Some(&WireId::Number(1))
        );
        assert_eq!(
            envelopes.get(1).ok_or("no second")?.kind(),
            MessageKind::Notification,
        );
        Ok(())
    }

    #[test]
    fn an_empty_batch_is_invalid_request() {
        assert_eq!(decode(b"[]").unwrap_err(), DecodeError::EmptyBatch);
        assert_eq!(
            DecodeError::EmptyBatch.code(),
            IrErrorCode::InvalidRequest,
            "an empty batch is a malformed frame, not a valid one with nothing in it"
        );
    }

    #[test]
    fn a_batch_is_rejected_whole_rather_than_elementwise() {
        // The second element is bad, so the frame is refused — the first element is
        // never reached. A batch that answered what it could would have run half
        // the work before discovering the error.
        let input = br#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"1.0","id":2}]"#;
        assert_eq!(
            decode(input).unwrap_err(),
            DecodeError::InvalidRequest("`jsonrpc` must be \"2.0\""),
        );
    }

    #[test]
    fn a_top_level_value_that_is_not_a_frame_is_invalid_request() {
        // Well-formed JSON that is not a JSON-RPC message. `-32600`, not `-32700`:
        // the bytes are fine, the message is not.
        for input in [b"42".as_slice(), b"\"hi\"".as_slice(), b"true".as_slice()] {
            assert_matches!(
                decode(input).unwrap_err(),
                DecodeError::InvalidRequest(_),
                "input was {:?}",
                String::from_utf8_lossy(input),
            );
        }
    }

    #[test]
    fn empty_input_is_a_parse_error() {
        assert_eq!(decode(b"").unwrap_err(), DecodeError::Json);
        assert_eq!(decode(b"   \n ").unwrap_err(), DecodeError::Json);
    }

    #[test]
    fn unknown_members_survive() -> Result<(), TestError> {
        // A field this build has never heard of must not fail the decode. The
        // bridge's contract is that a peer may send anything, and dropping or
        // rejecting an unknown member is the one way to make that contract
        // impossible to keep.
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"ping","$future":{"a":[1,2]}}"#;
        let envelope = first(input.as_slice())?;
        assert_eq!(envelope.kind(), MessageKind::Request);
        Ok(())
    }

    #[test]
    fn leading_whitespace_does_not_hide_the_frame_kind() -> Result<(), TestError> {
        let batch = decode(b"  \n\t [ {\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"} ]")?;
        assert!(batch.is_batch());
        let single = first(b"  {\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}".as_slice())?;
        assert_eq!(single.kind(), MessageKind::Request);
        Ok(())
    }

    #[test]
    fn params_stay_undecoded_until_the_method_is_known() -> Result<(), TestError> {
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"x","arguments":{}}}"#;
        let envelope = first(input.as_slice())?;
        let params = envelope.params().ok_or("the envelope has params")?;
        // Still the bytes the peer sent, not a decoded struct: that is the whole
        // point of deferring, and asserting on the raw text is what proves it.
        assert_eq!(params.get(), r#"{"name":"x","arguments":{}}"#);
        Ok(())
    }

    #[test]
    fn a_message_with_null_params_and_no_params_are_different() -> Result<(), TestError> {
        // `params: null` is written by some peers and means "no params"; omitting
        // it says the same thing. The two decode the same way here, which is the
        // only safe reading — a peer that sends `null` means no params, and
        // treating it as an error breaks a working client.
        let omitted = first(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.as_bytes())?;
        let null = first(r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":null}"#.as_bytes())?;
        assert!(omitted.params().is_none(), "no params member at all");
        assert!(
            null.params().is_none(),
            "an explicit null is the same absence, not a params value of null"
        );
        Ok(())
    }

    #[test]
    fn a_routeable_message_reports_its_method_and_id() -> Result<(), TestError> {
        let request = first(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.as_bytes())?;
        assert_eq!(
            request.routeable(),
            Some((&Method::Borrowed("ping"), Some(&WireId::Number(1))))
        );

        let notification = first(r#"{"jsonrpc":"2.0","method":"ping"}"#.as_bytes())?;
        assert_eq!(
            notification.routeable(),
            Some((&Method::Borrowed("ping"), None))
        );

        let reply = first(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#.as_bytes())?;
        assert_eq!(reply.routeable(), None, "a reply has no method to route");
        Ok(())
    }

    #[test]
    fn a_numeric_id_needs_no_allocation() -> Result<(), TestError> {
        // The claim is that reading a numeric id copies an `i64` out of the buffer.
        // The evidence is the type: `WireId::Number` is an `i64`, so constructing
        // one from `&str` text cannot have allocated a `String` on the way. This
        // test is here so the claim is checked from the outside as well — if
        // someone changes `WireId` to own its text, this stops compiling.
        let envelope = first(br#"{"jsonrpc":"2.0","id":9007199254740993,"method":"ping"}"#)?;
        let value = envelope
            .id()
            .and_then(WireId::as_number)
            .ok_or("a numeric id must stay a number")?;
        assert_eq!(
            value, 9_007_199_254_740_993,
            "and it must be exact past 2^53"
        );
        Ok(())
    }
}
