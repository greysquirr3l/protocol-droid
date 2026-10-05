//! The `text/event-stream` frame format, with no HTTP in it.
//!
//! # What this is for
//!
//! MCP's Streamable HTTP transport carries server-initiated messages as SSE.
//! The framing is small, the standard is precise about it, and every part of
//! it that is easy to get wrong is silent when it is wrong: a stream that
//! mis-frames still *looks* like a working stream until the one payload that
//! contains a newline arrives. So the format lives in its own file with no
//! `axum` in it, and the tests below are about the format rather than about
//! HTTP.
//!
//! The reference throughout is the WHATWG HTML standard's event-stream parsing
//! rules, which is the definition MCP's transport inherits. The three that
//! bite hardest:
//!
//! 1. **A line terminator is CRLF, CR or LF**, not just LF. A lone CR inside a
//!    `data:` line is a frame boundary to a conforming client, so an encoder
//!    that only splits on `\n` will happily emit a frame a client reads as two.
//! 2. **A newline in a payload is carried by repeating the field name.**
//!    `data: one\ndata: two\n\n` is one event whose data is `one\ntwo`. A raw
//!    newline in a `data:` line is not a payload character, it is the end of
//!    that line — so embedding one truncates the payload at the first newline
//!    and the client dispatches a short event. This is the single most common
//!    SSE encoder bug, and it corrupts a tool result that happens to contain
//!    one.
//! 3. **A line beginning with `:` is a comment** and dispatches nothing. That
//!    is how a keep-alive is expressed, and a decoder that turns a comment
//!    into an empty event hands its client a stream of empty messages.
//!
//! # Why the payload is a `serde_json::Value` and not a `String`
//!
//! Because the obvious shortcut is wrong in a way no test of the *transport*
//! will catch. `format!("{value:?}")` on a JSON string containing U+007F emits
//! `\u{7f}`, which is Rust's `Debug` escaping and is **not** JSON — a client
//! parsing the event fails, or worse, parses a truncated one. [`SseEvent::json`]
//! takes the value and serialises it with `serde_json`, so the only way to
//! build a frame from data is the way that produces legal JSON.
//!
//! # Round-trip, stated honestly
//!
//! [`encode`](SseEvent::encode) followed by [`decode`](SseEvent::decode) returns
//! the original payload exactly for any text with no CR in it. A CR does not
//! round-trip and cannot: the format has no escape for it, and a conformant
//! reader treats it as a line terminator, so the only faithful encoding of a
//! CR is to break the line and rejoin with LF. JSON never contains a raw CR —
//! `serde_json` escapes it as `\r` inside the string — so this is a statement
//! about the format's limits, not about a live loss. The test that proves it is
//! [`a_carriage_return_is_a_line_terminator`](self::tests::a_carriage_return_is_a_line_terminator).

use core::fmt::Write as _;
use core::ops::ControlFlow;
use core::str::FromStr;

use serde_json::Value;
use thiserror::Error;

use crate::domain::EventId;

/// The field name carrying the event id a client resumes from.
const ID_FIELD: &str = "id";

/// The field name carrying the payload.
const DATA_FIELD: &str = "data";

/// One `text/event-stream` frame.
///
/// `id` is optional because the format says so: a frame without an `id:` is
/// legal SSE and dispatches normally, it simply does not move the client's
/// resume cursor. That is the shape a keep-alive-adjacent event takes, and an
/// encoder that always wrote an `id` would either invent a cursor position
/// that never existed or refuse to emit the frame at all.
///
/// A frame *with* no `data` does not exist as a value: the standard dispatches
/// nothing for such a block, so [`decode`](Self::decode) reports
/// [`SseError::NoData`] and [`dispatch`](Self::dispatch) reports `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    id: Option<EventId>,
    data: String,
}

impl SseEvent {
    /// A frame with a resume cursor.
    #[must_use]
    pub const fn new(id: EventId, data: String) -> Self {
        Self { id: Some(id), data }
    }

    /// A frame with no resume cursor, which leaves the client's where it was.
    #[must_use]
    pub const fn without_id(data: String) -> Self {
        Self { id: None, data }
    }

    /// A frame whose data is `value` serialised as JSON.
    ///
    /// The only constructor that takes structured data, and the reason the
    /// `Debug`-formatting bug is unreachable rather than merely discouraged —
    /// see the module docs.
    ///
    /// # Errors
    ///
    /// [`SseError::Unserialisable`] if the value cannot be serialised, which
    /// for a [`Value`] means a non-string map key. The payload is the message
    /// the peer receives, so an event that cannot be encoded must not be
    /// encoded wrong.
    pub fn json(id: EventId, value: &Value) -> Result<Self, SseError> {
        let data = serde_json::to_string(value).map_err(|_| SseError::Unserialisable)?;
        Ok(Self::new(id, data))
    }

    /// The resume cursor, if this frame carries one.
    #[must_use]
    pub const fn id(&self) -> Option<EventId> {
        self.id
    }

    /// The payload, with the frame's line structure already undone.
    #[must_use]
    pub fn data(&self) -> &str {
        &self.data
    }

    /// Consumes the frame, yielding the payload.
    ///
    /// The id is dropped rather than returned because the write path uses the
    /// payload and the resume cursor comes from the ring that produced the
    /// event; a caller that wanted both has the [`StreamEvent`](crate::domain::StreamEvent)
    /// it came from.
    #[must_use]
    pub fn into_data(self) -> String {
        self.data
    }

    /// The bytes to write, without a trailing flush.
    ///
    /// `id: <n>\n` first, then one `data: ` line per line of the payload, then
    /// the blank line that dispatches the event. A payload with no newline in
    /// it is therefore exactly one `data:` line, and one with *k* newlines is
    /// *k + 1* — which is the claim
    /// [`a_newline_in_the_payload_becomes_another_data_line`](Self::tests::a_newline_in_the_payload_becomes_another_data_line)
    /// exists to hold.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = String::with_capacity(self.data.len() + 16);
        if let Some(id) = self.id {
            // `EventId` is a `u64` and `Display` writes decimal, so this
            // cannot fail and cannot produce anything a header would reject.
            let _ = writeln!(out, "{ID_FIELD}: {id}");
        }
        for_each_line(&self.data, |line| {
            out.push_str(DATA_FIELD);
            out.push_str(": ");
            out.push_str(line);
            out.push('\n');
            ControlFlow::Continue(())
        })
        .ok();
        // The blank line is what dispatches the event. Without it the client
        // buffers the frame indefinitely and the stream looks alive but is not.
        out.push('\n');
        out
    }

    /// Parses one frame's block — its lines, without the trailing blank.
    ///
    /// # Errors
    ///
    /// [`SseError::NoData`] if the block carries no `data:` field, and
    /// [`SseError::MalformedId`] if an `id:` field is not a decimal event id.
    pub fn decode(block: &str) -> Result<Self, SseError> {
        Self::dispatch(block)?.ok_or(SseError::NoData)
    }

    /// The standard's dispatch, which dispatches *nothing* for some blocks.
    ///
    /// Returns `None` for a block that is empty or is entirely comments, and
    /// that is the load-bearing difference from [`decode`](Self::decode): a
    /// keep-alive comment is a legal block that produces no event, and a stream
    /// loop that treated it as one would hand its client an empty message
    /// every heartbeat.
    ///
    /// # Errors
    ///
    /// [`SseError::MalformedId`] if an `id:` field is not a decimal event id.
    /// A bad cursor is an error rather than a zero: `EventId`'s `0` is a real,
    /// meaningful watermark meaning "before the first event", and quietly
    /// reading a corrupt id as that would tell a client it has seen everything
    /// when it has seen nothing.
    pub fn dispatch(block: &str) -> Result<Option<Self>, SseError> {
        let mut id: Option<EventId> = None;
        // Accumulated as a `Vec<&str>` of lines and joined once, rather than
        // pushed as a running string: the join is the only place a separator
        // can be wrong, and a single `join` makes it one place.
        let mut data: Vec<&str> = Vec::new();

        for_each_line(block, |line| {
            // A leading colon is a comment, by definition. The standard says so
            // before it says anything about fields, and it is how keep-alives
            // are written.
            if line.starts_with(':') {
                return ControlFlow::Continue(());
            }
            // A line with no colon is a field name with an empty value, which
            // the standard defines rather than treats as malformed.
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (
                    field,
                    // Exactly one leading space is part of the framing, not the
                    // value. A second one is the payload's, and a session id
                    // with a stray space in it is a client's typo to report
                    // rather than to paper over here.
                    value.strip_prefix(' ').unwrap_or(value),
                ),
                None => (line, ""),
            };
            match field {
                ID_FIELD => match parse_event_id(value) {
                    Ok(parsed) => id = Some(parsed),
                    // Stop at the first bad field rather than carrying on: a
                    // block with a corrupt cursor is not a frame this decoder
                    // is entitled to interpret, because the cursor decides what
                    // the client believes it has already received.
                    Err(error) => return ControlFlow::Break(error),
                },
                DATA_FIELD => data.push(value),
                // `event`, `retry` and anything a future revision adds are
                // ignored, per the standard. MCP does not use them.
                _ => {}
            }
            ControlFlow::Continue(())
        })?;

        if data.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            id,
            data: data.join("\n"),
        }))
    }
}

/// Reads an `id:` field value as an event id.
///
/// The only field MCP writes is a decimal `u64`, which is exactly what
/// [`EventId`]'s `Display` produces, so a value that does not parse came from
/// somewhere other than this encoder — a proxy that rewrote the header, a
/// client that invented one, a future revision using a different scheme. All
/// three are worth reporting rather than guessing at.
fn parse_event_id(value: &str) -> Result<EventId, SseError> {
    u64::from_str(value).map_or(Err(SseError::MalformedId), |id| Ok(EventId::new(id)))
}

/// Calls `visit` once per line, treating CRLF, CR and LF as terminators.
///
/// The CR arm is the one worth reading twice. A client that splits only on LF
/// will see a lone CR as an ordinary character and keep the rest of the line,
/// while this function and any conforming reader treat it as a break — so an
/// encoder that ignored it would produce a frame the two sides read
/// differently. The frame is already written, so nothing here can recover.
///
/// The index only ever advances past ASCII bytes, so every range it produces
/// lands on a UTF-8 character boundary and `str::get` cannot return `None`
/// here. It is still a `get` rather than a slice: a range index that is wrong
/// by one is a panic on a live stream, and the cost of the alternative is a
/// branch the optimiser removes.
/// Returns the error the visitor broke with, or `Ok(())` if it ran to the end.
///
/// A `ControlFlow` rather than a plain `for_each` so a parse failure inside
/// the visitor can stop the walk: the `id:` field is parsed as it is met, and
/// continuing past a corrupt one to collect the rest of the block would do
/// work whose result is already discarded.
///
/// The `for<'a>` on the visitor is what lets a caller push the borrowed lines
/// out of the closure — the parameter's lifetime is the *input's*, not a fresh
/// anonymous one, so a `&str` handed to the visitor is still a `&str` the
/// caller can keep.
fn for_each_line<'i>(
    input: &'i str,
    mut visit: impl FnMut(&'i str) -> ControlFlow<SseError>,
) -> Result<(), SseError> {
    let mut start = 0;
    let mut index = 0;
    while let Some(byte) = input.as_bytes().get(index) {
        match *byte {
            b'\n' => {
                if let Some(line) = input.get(start..index)
                    && let ControlFlow::Break(error) = visit(line)
                {
                    return Err(error);
                }
                index += 1;
                start = index;
            }
            b'\r' => {
                if let Some(line) = input.get(start..index)
                    && let ControlFlow::Break(error) = visit(line)
                {
                    return Err(error);
                }
                index += 1;
                // CRLF is one terminator, not two. Skipping the LF is what
                // stops a CRLF-encoded payload from gaining a blank line per
                // line and dispatching an empty event for each.
                if input.as_bytes().get(index) == Some(&b'\n') {
                    index += 1;
                }
                start = index;
            }
            _ => index += 1,
        }
    }
    // The trailing piece is emitted even when it is empty, and that is
    // load-bearing in both directions. Emptily: an empty payload is one
    // empty `data:` line, so without this a payload of `""` would encode to a
    // frame the decoder refuses as carrying no data at all. Trailingly: a
    // payload ending in a newline has an empty *last* line, which is how the
    // final newline survives the trip — `str::split` semantics, and the same
    // rule the decoder's buffer-then-strip-one-LF arithmetic assumes.
    if let Some(last) = input.get(start..)
        && let ControlFlow::Break(error) = visit(last)
    {
        return Err(error);
    }
    Ok(())
}

/// Why a frame could not be read or written.
///
/// The variants are what a caller can act on separately: a frame that cannot be
/// serialised is this process's bug, a corrupt id is the peer's, and a block
/// with no data is not a failure at all — the standard simply dispatches
/// nothing for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SseError {
    /// The block carried no `data:` field, so the standard dispatches no event.
    #[error("this block carries no data field, so it dispatches no event")]
    NoData,

    /// An `id:` field was not a decimal event id.
    ///
    /// Carries nothing from the input on purpose. The value came from a peer,
    /// it may be arbitrarily long, and a refusal that echoes it would be
    /// reflecting untrusted input into a response body for no diagnostic gain:
    /// the peer knows what it sent.
    #[error("the id field is not a decimal event id")]
    MalformedId,

    /// The value has a map key that is not a string, so it has no JSON form.
    #[error("the payload cannot be serialised as JSON")]
    Unserialisable,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{SseError, SseEvent, for_each_line};
    use crate::domain::EventId;

    type TestError = Box<dyn std::error::Error>;

    /// Collects the lines `for_each_line` reports, for the terminator tests.
    fn split(input: &str) -> Vec<&str> {
        let mut lines = Vec::new();
        let _ = for_each_line(input, |line| {
            lines.push(line);
            core::ops::ControlFlow::Continue(())
        });
        lines
    }

    /// One `data:` line, when the payload has no newline in it.
    ///
    /// This is the baseline the newline test is measured against: an encoder
    /// that always wrote a fixed number of `data:` lines would satisfy a
    /// "contains `data:`" assertion and still be wrong.
    #[test]
    fn a_single_line_payload_is_one_data_line() {
        let frame = SseEvent::new(EventId::new(7), r#"{"jsonrpc":"2.0"}"#.to_owned());
        assert_eq!(frame.encode(), "id: 7\ndata: {\"jsonrpc\":\"2.0\"}\n\n");
    }

    /// A newline in the payload becomes another `data:` line, never a raw one.
    ///
    /// The mutation this kills is the whole reason the module exists: writing
    /// `data: one\ntwo\n\n` and calling it a frame. That output looks correct
    /// in a log, and a conforming client reads it as a one-line event
    /// followed by a field named `two` — so a tool result containing a newline
    /// arrives truncated, with no error anywhere.
    ///
    /// The assertion is on the *count* of `data:` lines and on there being no
    /// bare line between them, because either alone is satisfiable by an
    /// encoder that merely happens to agree on this one input.
    #[test]
    fn a_newline_in_the_payload_becomes_another_data_line() {
        let frame = SseEvent::new(EventId::new(1), "one\ntwo\nthree".to_owned());
        let encoded = frame.encode();
        assert_eq!(
            encoded, "id: 1\ndata: one\ndata: two\ndata: three\n\n",
            "a payload newline must be carried by repeating the field name"
        );
        assert_eq!(encoded.matches("data: ").count(), 3);
        assert!(
            !encoded.contains("one\ntwo"),
            "a raw newline reached the wire inside a data line"
        );
    }

    /// Encode then decode returns the payload, for the shapes that can.
    ///
    /// Table-driven because the interesting cases are unrelated to each other:
    /// an empty payload, a payload that is only a newline, quotes and
    /// backslashes, non-ASCII text, and a DEL. The last is the one that proves
    /// the `Debug`-escaping bug is unreachable — `format!("{value:?}")` turns
    /// U+007F into `\u{7f}`, which is not JSON, and the round trip here would
    /// come back with those six literal characters in it.
    #[test]
    fn encode_then_decode_round_trips() -> Result<(), TestError> {
        let payloads = [
            String::new(),
            "a".to_owned(),
            "\n".to_owned(),
            "\n\n".to_owned(),
            "line one\nline two".to_owned(),
            r#"{"a":"b\"c\\d"}"#.to_owned(),
            "café — ünïcode".to_owned(),
            "\u{7f}".to_owned(),
            "trailing\n".to_owned(),
        ];
        for payload in payloads {
            let frame = SseEvent::new(EventId::new(3), payload.clone());
            let decoded = SseEvent::decode(&frame.encode())?;
            assert_eq!(decoded.data(), payload, "round trip changed {payload:?}");
            assert_eq!(decoded.id(), Some(EventId::new(3)));
        }
        Ok(())
    }

    /// A payload with k newlines is carried by k + 1 `data:` lines, always.
    ///
    /// The generalisation of the single case, checked at a size where an
    /// off-by-one in the splitter would show up. A trailing newline is the
    /// awkward one: it must produce a final *empty* `data:` line, or the
    /// payload's final LF is lost.
    #[test]
    fn the_data_line_count_is_the_newline_count_plus_one() -> Result<(), TestError> {
        for newlines in 0..5_usize {
            // A payload with exactly `newlines` newline characters in it, so
            // the loop variable is the count the invariant is about.
            let payload = "x\n".repeat(newlines);
            let encoded = SseEvent::without_id(payload.clone()).encode();
            assert_eq!(
                encoded.matches("data: ").count(),
                newlines + 1,
                "a payload of {newlines} newlines was mis-framed: {encoded:?}"
            );
            assert_eq!(SseEvent::decode(&encoded)?.data(), payload);
        }
        Ok(())
    }

    /// A lone CR is a line terminator on the wire and an LF after decoding.
    ///
    /// States the format's limit rather than hiding it: the round trip is exact
    /// for LF and LF/CRLF text and *normalises* a bare CR, because the format
    /// has no escape for one and a reader that treated it as data would
    /// disagree with every other reader. JSON cannot contain a raw CR, so this
    /// costs nothing in practice.
    #[test]
    fn a_carriage_return_is_a_line_terminator() -> Result<(), TestError> {
        // Split semantics throughout: a trailing terminator leaves an empty
        // final piece, which is exactly what carries the payload's last
        // newline back out of the encoder.
        assert_eq!(split("a\rb"), vec!["a", "b"]);
        assert_eq!(split("a\r\nb"), vec!["a", "b"]);
        assert_eq!(split("a\nb"), vec!["a", "b"]);
        assert_eq!(split("a\n"), vec!["a", ""]);
        assert_eq!(split("a\r\n"), vec!["a", ""]);
        assert_eq!(split("a\r"), vec!["a", ""]);
        assert_eq!(split("a\n\n"), vec!["a", "", ""]);
        // No terminator at all is one line, and nothing at all is one empty one.
        assert_eq!(split("a"), vec!["a"]);
        assert_eq!(split(""), vec![""]);

        let frame = SseEvent::without_id("one\rtwo".to_owned());
        assert_eq!(frame.encode(), "data: one\ndata: two\n\n");
        assert_eq!(SseEvent::decode(&frame.encode())?.data(), "one\ntwo");
        Ok(())
    }

    /// CRLF does not add an empty data line per line.
    ///
    /// A decoder that split on CR and then on LF independently would see a
    /// blank line between every pair and dispatch an empty event for each — a
    /// stream where every real message is followed by a phantom one.
    #[test]
    fn crlf_encoded_frames_dispatch_exactly_one_event() -> Result<(), TestError> {
        let frame = SseEvent::new(EventId::new(4), "one\ntwo".to_owned());
        let crlf = frame.encode().replace('\n', "\r\n");
        let decoded = SseEvent::decode(&crlf)?;
        assert_eq!(decoded.data(), "one\ntwo");
        assert_eq!(decoded.id(), Some(EventId::new(4)));
        Ok(())
    }

    /// A frame with no `id:` is legal and decodes with no cursor.
    ///
    /// The standard does not require an id, and MCP's own streams do not always
    /// have one to give. An encoder or decoder that insisted would either
    /// invent a cursor position that never existed — and then a resume from it
    /// would be refused as `Unknown` — or drop the frame.
    #[test]
    fn a_frame_with_no_id_decodes_with_no_id() -> Result<(), TestError> {
        let frame = SseEvent::without_id("{\"a\":1}".to_owned());
        let encoded = frame.encode();
        assert_eq!(encoded, "data: {\"a\":1}\n\n");
        let decoded = SseEvent::decode(&encoded)?;
        assert_eq!(decoded.id(), None);
        assert_eq!(decoded.data(), "{\"a\":1}");
        Ok(())
    }

    /// A comment line dispatches nothing, and is not an empty event.
    ///
    /// This is the keep-alive. A server that writes `:` on an idle connection
    /// is saying "still here" and must not produce an event; a decoder that
    /// turned it into one would hand the client a message with no id and no
    /// content on every heartbeat, forever.
    #[test]
    fn a_comment_line_dispatches_nothing() -> Result<(), TestError> {
        for block in [":", ": keep-alive", "", "\n\n", ": one\n: two\n\n"] {
            assert_eq!(
                SseEvent::dispatch(block)?,
                None,
                "a comment-only block dispatched an event"
            );
            assert_eq!(SseEvent::decode(block), Err(SseError::NoData));
        }
        // And a comment inside a real frame is skipped without disturbing it.
        let frame = SseEvent::decode("id: 2\n: a comment\ndata: payload")?;
        assert_eq!(frame.id(), Some(EventId::new(2)));
        assert_eq!(frame.data(), "payload");
        Ok(())
    }

    /// A `Last-Event-ID` value that is not a number is an error, not a zero.
    ///
    /// The mutation this kills is a `unwrap_or_default()` on the parse, which
    /// reads any corrupt cursor as `EventId::WATERMARK` — "before the first
    /// event". That is a real, meaningful id, so the error would not surface:
    /// the ring would be asked to replay from the start of time and answer
    /// `UpToDate` for a client that has actually seen nothing, and the stream
    /// would sit silent forever while the client believes it is current.
    #[test]
    fn a_corrupt_id_is_an_error_and_not_a_zero() -> Result<(), TestError> {
        for block in [
            "id: abc\ndata: x",
            "id: \ndata: x",
            "id: 12x\ndata: x",
            "id: 1.0\ndata: x",
            "id: 99999999999999999999\ndata: x",
        ] {
            assert_eq!(
                SseEvent::decode(block),
                Err(SseError::MalformedId),
                "{block:?} was accepted"
            );
        }
        // Zero, on the other hand, is a legitimate cursor and must survive.
        assert_eq!(
            SseEvent::decode("id: 0\ndata: x")?.id(),
            Some(EventId::new(0))
        );
        Ok(())
    }

    /// A payload built from JSON is legal JSON on the wire.
    ///
    /// The DEL is the case: `serde_json` emits it raw, because U+007F is legal
    /// inside a JSON string unescaped, and `format!("{value:?}")` would emit
    /// `\u{7f}` — six characters that are not a DEL and are not valid JSON
    /// escape syntax outside a Rust debug context. Asserting the round trip
    /// through `serde_json::from_str` is what makes the claim about *validity*
    /// rather than about *equality with a Debug string*.
    #[test]
    fn a_json_payload_is_parsed_as_json_by_a_reader() -> Result<(), TestError> {
        let value = json!({"text": "before\u{7f}after", "n": 1});
        let frame = SseEvent::json(EventId::new(9), &value)?;
        let encoded = frame.encode();
        assert!(
            !encoded.contains("\\u{"),
            "the payload carries Rust debug escaping: {encoded:?}"
        );

        let decoded = SseEvent::decode(&encoded)?;
        let reparsed: Value = serde_json::from_str(decoded.data())?;
        assert_eq!(reparsed, value);
        Ok(())
    }

    /// Unknown fields are ignored, not preserved and not fatal.
    ///
    /// `event` and `retry` are part of the standard and MCP does not use them;
    /// anything else is a future revision's field. The standard says to ignore
    /// what you do not understand, and this is the assertion that the decoder
    /// follows it — a decoder that errored would break against a conforming
    /// client that sets `retry`.
    #[test]
    fn unknown_fields_are_ignored() -> Result<(), TestError> {
        let frame =
            SseEvent::decode("event: message\nretry: 5000\nid: 5\nwhatever: x\ndata: payload")?;
        assert_eq!(frame.id(), Some(EventId::new(5)));
        assert_eq!(frame.data(), "payload");
        Ok(())
    }

    /// Only the framing space after the colon is stripped, never the payload's.
    ///
    /// One leading space belongs to the format. A second is data, and a payload
    /// that begins with a space is common enough in JSON-adjacent text that
    /// trimming it would corrupt the message. Symmetrically, a field with no
    /// colon at all is a field with an empty value rather than a parse error,
    /// which is what the standard specifies.
    #[test]
    fn exactly_one_space_after_the_colon_is_framing() -> Result<(), TestError> {
        assert_eq!(SseEvent::decode("data:  two")?.data(), " two");
        assert_eq!(SseEvent::decode("data:two")?.data(), "two");
        assert_eq!(
            SseEvent::dispatch("data")?,
            Some(SseEvent::without_id(String::new()))
        );
        Ok(())
    }

    /// A frame survives being concatenated into a stream and read back whole.
    ///
    /// The encoder is per-frame, but the bytes are a stream, so the property
    /// that matters is that concatenating encoded frames yields a block
    /// sequence a decoder reads as the same events. An encoder that omitted the
    /// dispatching blank line would pass every single-frame test above and
    /// fail this one.
    #[test]
    fn concatenated_frames_read_back_as_the_same_events() -> Result<(), TestError> {
        let frames = [
            SseEvent::new(EventId::new(1), "one".to_owned()),
            SseEvent::new(EventId::new(2), "two\nlines".to_owned()),
            SseEvent::without_id("three".to_owned()),
        ];
        let stream: String = frames.iter().map(SseEvent::encode).collect();

        let mut decoded = Vec::new();
        for block in stream.trim_end_matches('\n').split("\n\n") {
            if let Some(event) = SseEvent::dispatch(block)? {
                decoded.push(event);
            }
        }
        assert_eq!(decoded, frames.to_vec());
        Ok(())
    }
}
