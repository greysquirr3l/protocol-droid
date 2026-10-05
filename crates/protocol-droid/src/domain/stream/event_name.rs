//! The `event:` field of a `text/event-stream` frame.
//!
//! # Why this is a newtype and not a `String`
//!
//! Because a name is a wire token, and a wire token that is spliced into a
//! frame without being checked is an injection primitive. The encoder writes
//!
//! ```text
//! event: <name>
//! ```
//!
//! and a `name` containing a newline therefore does not become a name that
//! renders oddly — it becomes a *second field*, and everything after the break
//! is re-read by the client as a field of its own. A name of
//! `message\ndata: {"jsonrpc":"2.0"}` is a name that carries a payload, and a
//! payload that the peer never sent.
//!
//! That is not a hypothetical: the legacy transport's names come from a
//! [`MessageEndpoint`](crate::domain::stream::event_name::MessageEndpoint) the
//! server built and a tool name a caller chose, and a tool name is a string
//! somebody typed. The parse is where the check belongs — the crate's rule is
//! to parse rather than to validate, and this is a parse.
//!
//! # What the standard actually requires
//!
//! The WHATWG event-stream grammar makes `event` a field whose value is the
//! rest of the line. It does not forbid a colon in the value, because a colon
//! cannot terminate a line — so `a:b` is a legal name. What the grammar cannot
//! express, and what this type rejects, is a value containing a line
//! terminator, because the grammar has no escaping for one and the only
//! faithful encoding of an embedded break is to emit the break.

use core::fmt;
use core::str::FromStr;
use std::borrow::Cow;

use thiserror::Error;

/// The `event` name a legacy-transport frame carries, or the unnamed default.
///
/// The two names the `2024-11-05` HTTP+SSE transport defines, kept as
/// constants so no call site spells them as a literal. The specification
/// names them in prose — "the server MUST send an `endpoint` event" and
/// "server messages are sent as SSE `message` events" — and a literal at a
/// call site is how one of them ends up spelled `Endpoint` and silently
/// matches nothing.
///
/// Cited: <https://modelcontextprotocol.io/specification/2024-11-05/basic/transports>
pub mod names {
    /// The first event on a legacy stream: where to POST.
    pub const ENDPOINT: super::EventName = super::EventName::const_new("endpoint");

    /// Every JSON-RPC message the server sends back.
    pub const MESSAGE: super::EventName = super::EventName::const_new("message");
}

/// A validated `event:` value.
///
/// A [`Cow`] rather than an `Arc<str>` for one reason that turned out to
/// decide the design: `Arc::from` is not `const`, so an `Arc`-backed name
/// cannot be a `const`, and the two names the legacy transport is defined by
/// are exactly the kind of thing that should be a constant rather than a value
/// built at first use. `Cow::Borrowed` *is* const-constructible, so the two
/// names the specification names cost nothing and compare without a pointer
/// chase, while a name that arrived from a peer is owned and costs one
/// allocation — which is right, because a peer's name is not known ahead of
/// time and there is nothing to borrow it from.
///
/// `Borrowed` is safe here for the same reason it is safe anywhere: a `&'static
/// str` has no lifetime to shorten, and `from_str` is the only constructor
/// that produces a `Borrowed` variant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventName(Cow<'static, str>);

impl EventName {
    /// A compile-time name, for the constants the legacy transport uses.
    ///
    /// `const` because those are the only names this crate writes itself, and a
    /// name that needed a parse to be written would be a name the crate could
    /// get wrong. Only a `&'static str` can be passed, so a name with a
    /// lifetime — a tool name held across an await, say — cannot be quietly
    /// promoted into a constant that outlives it.
    ///
    /// `pub(crate)` and not `pub` for the reason the whole type exists: a
    /// `const fn` cannot scan for a line terminator, so this constructor is
    /// the one door that bypasses the check. Making it crate-private leaves
    /// [`from_str`](Self::from_str) as the only way a name can enter from
    /// outside this crate, and leaves exactly two callers inside it — the two
    /// constants below, which are literals and are asserted by a test. A
    /// `pub const_new` would hand every caller the injection this newtype
    /// closes, in a form that looked like the safe constructor.
    pub(crate) const fn const_new(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }

    /// The name as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is `name`.
    ///
    /// Compares contents, not representations: a `Borrowed("message")` and an
    /// owned `"message"` are the same name, and a client that dispatched on
    /// which one it was would be matching on an implementation detail.
    #[must_use]
    pub fn is(&self, name: &Self) -> bool {
        self.0 == name.0
    }
}

/// Why a string is not usable as an `event:` name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EventNameError {
    /// The value was empty.
    ///
    /// `event:` with nothing after it is a field with an empty value, which the
    /// grammar allows and which means the *default* event name rather than the
    /// empty one. Accepting it would make `Some("")` and `None` two spellings
    /// of one thing, and a client dispatching on the name would have to treat
    /// them differently for no reason.
    #[error("an event name may not be empty")]
    Empty,

    /// The value contained a line terminator.
    ///
    /// The load-bearing refusal. See the module docs: this is what would turn
    /// a name into a second field.
    #[error("an event name may not contain a line terminator")]
    LineTerminator,
}

impl FromStr for EventName {
    type Err = EventNameError;

    /// Parses a name, refusing the one thing that would inject a field.
    ///
    /// CR and LF are both rejected, and both must be: the encoder's line
    /// terminator is LF, but a conformant *reader* also treats a lone CR as a
    /// break, so a name holding a CR reads as two fields on a reader that the
    /// writer did not have in mind. Rejecting only LF would leave that reader
    /// injectable.
    ///
    /// A colon is **not** rejected. The grammar makes the value the rest of the
    /// line, so `a:b` is a legal name and a client comparing it to `b` has a
    /// bug, not a server that corrupted it.
    ///
    /// # Errors
    ///
    /// [`EventNameError::Empty`] for an empty value, and
    /// [`EventNameError::LineTerminator`] for one containing CR or LF.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(EventNameError::Empty);
        }
        if value.contains(['\n', '\r']) {
            return Err(EventNameError::LineTerminator);
        }
        Ok(Self(Cow::Owned(value.to_string())))
    }
}

impl fmt::Display for EventName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{EventName, EventNameError, names};
    use core::str::FromStr;

    /// What a test here can fail with.
    ///
    /// A test's own complaint, or the parse's own error. Two unrelated error
    /// types in one test module is the case the crate's own testing rules call
    /// out — a single `enum` with a `From` each, rather than a `Box<dyn
    /// Error>` that accepts anything and so converts nothing usefully.
    #[derive(Debug)]
    enum Failure {
        /// The test's own complaint.
        Complaint(String),
        /// A parse the test wanted to succeed refused.
        Refused(EventNameError),
    }

    impl From<EventNameError> for Failure {
        fn from(error: EventNameError) -> Self {
            Self::Refused(error)
        }
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Complaint(text) => f.write_str(text),
                Self::Refused(error) => write!(f, "refused when a name was wanted: {error}"),
            }
        }
    }

    type TestError = Failure;

    /// The test's own complaint.
    fn fail(message: impl Into<String>) -> TestError {
        Failure::Complaint(message.into())
    }

    /// Asserts that `value` parses to a name reading back as `expected`.
    fn parses_to(value: &str, expected: &str) -> Result<(), TestError> {
        let name = EventName::from_str(value)?;
        if name.as_str() != expected {
            return Err(fail(format!(
                "{value:?} parsed as {:?}, not {expected:?}",
                name.as_str()
            )));
        }
        Ok(())
    }

    /// Asserts that `value` is refused, and with `expected`.
    fn refused_with(value: &str, expected: &EventNameError) -> Result<(), TestError> {
        match EventName::from_str(value) {
            Err(error) if &error == expected => Ok(()),
            Err(error) => Err(fail(format!(
                "{value:?} was refused as {error:?}, not {expected:?}"
            ))),
            Ok(name) => Err(fail(format!(
                "{value:?} parsed as {:?}, which cannot be encoded safely",
                name.as_str()
            ))),
        }
    }

    #[test]
    fn a_plain_name_parses() -> Result<(), TestError> {
        parses_to("endpoint", "endpoint")
    }

    /// A colon is the rest of the line, and the grammar says so before it says
    /// anything about which characters a name may contain. A client that
    /// compared `a:b` to `b` has a bug; a server that refused to send it would
    /// be refusing a legal frame.
    #[test]
    fn a_colon_is_legal_because_the_value_is_the_rest_of_the_line() -> Result<(), TestError> {
        parses_to("a:b", "a:b")
    }

    /// The refusal that is the whole reason this type exists.
    ///
    /// Without it, `event: message\ndata: {...}` reaches a client as a name
    /// and a payload, and the client dispatches a message the peer never sent.
    #[test]
    fn a_newline_in_a_name_is_refused() -> Result<(), TestError> {
        refused_with(
            "message\ndata: {\"jsonrpc\":\"2.0\"}",
            &EventNameError::LineTerminator,
        )
    }

    /// A lone CR is a terminator to a conformant *reader* even though the
    /// encoder writes LF, so the parse has to reject it too. A reader that
    /// only splits on LF would otherwise see the name and the payload as one
    /// line, and the writer would have disagreed with a reader it cannot see.
    #[test]
    fn a_carriage_return_in_a_name_is_refused() -> Result<(), TestError> {
        refused_with("message\rdata: x", &EventNameError::LineTerminator)
    }

    /// Empty is refused so that `Some("")` and `None` cannot both mean the
    /// default name. The grammar allows an empty value; two spellings of one
    /// meaning is where a dispatch comparison goes wrong.
    #[test]
    fn an_empty_name_is_refused() -> Result<(), TestError> {
        refused_with("", &EventNameError::Empty)
    }

    /// The constants have to be the names on the wire, spelled exactly, and a
    /// constant that did not compare equal to a parsed name would silently
    /// match nothing in every dispatch that used it.
    #[test]
    fn the_legacy_names_are_the_specified_spellings() -> Result<(), TestError> {
        if names::ENDPOINT.as_str() != "endpoint" {
            return Err(fail("the endpoint constant is not spelled `endpoint`"));
        }
        if names::MESSAGE.as_str() != "message" {
            return Err(fail("the message constant is not spelled `message`"));
        }
        let parsed = EventName::from_str("message")?;
        if !parsed.is(&names::MESSAGE) {
            return Err(fail("a parsed name does not match the message constant"));
        }
        if names::MESSAGE.is(&names::ENDPOINT) {
            return Err(fail("the two legacy names are equal"));
        }
        Ok(())
    }
}
