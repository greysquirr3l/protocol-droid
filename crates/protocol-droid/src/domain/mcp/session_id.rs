//! `McpSessionId` — the `Mcp-Session-Id` header value, as a value object.
//!
//! # Where the two requirements come from, and they are not the same one
//!
//! The visible-ASCII rule is **HTTP's**, not MCP's. `Mcp-Session-Id` is a header
//! field-value, and RFC 9110 §5.5 defines a field-value over `field-vchar`,
//! which is `%x21-7E` plus SP — `%x20-7E` in the form the task brief states. Any
//! implementation that puts a non-visible-ASCII byte in a header is not sending
//! a header at all. That is the reason this type validates, and it is a
//! transport requirement that would hold for any header this crate ever sends.
//!
//! The 16-digit hexadecimal spelling is **ours**. The specification that
//! introduced this header was removed in `2026-07-28` (major change 1, SEP-2567,
//! `crates/protocol-droid-schema/spec/2026-07-28/CHANGELOG.md`) without ever
//! fixing a format for it, and no earlier vendored revision states one. So the
//! encoding here is a decision, documented below, rather than a fact read out of
//! the schema — and a peer is required to treat it as opaque either way.
//!
//! # Why the format is hexadecimal, and why that is not a security problem
//!
//! `SessionId` is a `u64`, so *any* encoding of it is at most 64 bits and the
//! encoding cannot add entropy. Rendering it as 16 lowercase-free hex digits
//! costs one stack buffer and no allocation on a path that runs on every
//! response header, via [`core::fmt::NumBuffer`]. A denser base-32 spelling would
//! save three characters and buy nothing an attacker could not also get from the
//! `u64` underneath.
//!
//! Enumerable session ids are acceptable here because the specification does not
//! make the id a capability, and this crate says so at the source:
//! [`IdSource`](crate::application::session::IdSource) documents that an id "is
//! not a capability, not a secret, and not something a peer is ever asked to
//! trust". A deployment that needs unguessable ids seeds the `IdSource` with
//! more entropy; it cannot get them from the encoding.
//!
//! # Why this is a separate newtype rather than a rendering of `SessionId`
//!
//! Because the two make different promises. `SessionId` is the internal identity
//! the event log correlates and the redaction rule truncates, and it makes no
//! claim about bytes a peer can send back. This type makes a claim about a
//! header grammar, and it has to be able to *reject* — a peer's `Mcp-Session-Id`
//! arrives as arbitrary text and must be parsed. A type that cannot be
//! constructed from text cannot reject anything, and a `SessionId` that
//! validated `%x20-7E` would be validating a constraint no internal caller has
//! any reason to respect.
//!
//! The mapping between them is a bijection, which is what makes the design
//! worth the extra type: every `McpSessionId` names exactly one `SessionId`, so
//! an inbound header always resolves to a session the server actually has, and
//! no wire id can exist that no session owns. A second minted id instead of an
//! encoding would break exactly that property — there would be ids in the
//! header space with no session behind them, and a lookup that had to handle it.

use core::fmt;
use core::num::ParseIntError;
use core::str::FromStr;

use thiserror::Error;

use crate::domain::session::SessionId;

/// The byte range an `Mcp-Session-Id` value is drawn from, inclusive.
///
/// RFC 9110 §5.5 `field-vchar` plus SP. Exposed so the test that proves the
/// encoding is in range and the parser that enforces it cite the same two
/// numbers instead of each spelling their own.
pub const VISIBLE_ASCII_RANGE: core::ops::RangeInclusive<u8> = 0x20..=0x7E;

/// How many characters [`McpSessionId`] renders.
///
/// A `u64` needs sixteen hex digits, and sixteen is fixed rather than minimal so
/// that two ids can never differ in length — a length-sensitive intermediary
/// between us and the client would otherwise see `7` and `0007` as different
/// sessions.
const DIGITS: usize = 16;

/// The `Mcp-Session-Id` header value naming one session.
///
/// `Copy`, because the only field is a [`SessionId`], which is `Copy`. It is
/// therefore a plain `u64` on the wire path, which is the point: this sits in
/// the per-request state and is compared against on every request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpSessionId(SessionId);

impl McpSessionId {
    /// The wire id for an internal session.
    ///
    /// Infallible, and it is infallible by construction rather than by trusting
    /// the caller: the rendering is a fixed 16-digit hex expansion of a `u64`,
    /// every one of which is inside [`VISIBLE_ASCII_RANGE`].
    #[must_use]
    pub const fn new(session: SessionId) -> Self {
        Self(session)
    }

    /// The internal session this header names.
    ///
    /// The inverse of [`new`](Self::new), and total: every id this type can
    /// hold has a session, because the id *is* a session.
    #[must_use]
    pub const fn session(self) -> SessionId {
        self.0
    }

    /// The value as bytes, for an adapter filling in a header.
    ///
    /// Takes a buffer rather than returning a `String` because the alternative
    /// allocates once per response and this is on every one of them. The buffer
    /// only has to outlive the borrow it returns.
    #[must_use]
    pub fn as_bytes(self, buffer: &mut [u8; DIGITS]) -> &[u8; DIGITS] {
        // Written out nibble by nibble rather than through `format_into`,
        // which renders decimal: the header is sixteen hex digits, zero-padded,
        // and a decimal rendering of the same number is a different value to
        // whatever parses it back. The width is fixed by the loop, so it cannot
        // drift from [`DIGITS`], and every digit it writes is a hex letter or
        // an arabic numeral — inside [`VISIBLE_ASCII_RANGE`] by construction.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut remaining = self.0.get();
        for slot in buffer.iter_mut().rev() {
            // `remaining & 0xF` is 0..=15 by construction, so the conversion
            // cannot fail; the fallback is here so a widening integer type
            // would be a slow default rather than a panic on the request path.
            let index = usize::from(u8::try_from(remaining & 0xF).unwrap_or(0));
            *slot = HEX.get(index).copied().unwrap_or(b'0');
            remaining >>= 4;
        }
        buffer
    }
}

impl fmt::Display for McpSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buffer = [0u8; DIGITS];
        formatter.write_str(core::str::from_utf8(self.as_bytes(&mut buffer)).unwrap_or_default())
    }
}

impl From<SessionId> for McpSessionId {
    fn from(session: SessionId) -> Self {
        Self(session)
    }
}

impl From<McpSessionId> for SessionId {
    fn from(id: McpSessionId) -> Self {
        id.0
    }
}

impl FromStr for McpSessionId {
    type Err = McpSessionIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let value = McpSessionIdError::validate(text)?;
        let digits = u64::from_str_radix(value, 16)
            .map_err(|source| McpSessionIdError::Unparsable { source })?;
        Ok(Self(SessionId::new(digits)))
    }
}

/// Why a `Mcp-Session-Id` header is not an [`McpSessionId`].
///
/// Four distinct refusals rather than one, because they are four different
/// mistakes and a caller that gets one of them back needs to know which. A
/// single "invalid" string would make the common case — a client that sent the
/// wrong header name, or one that pre-dates the session concept — impossible to
/// tell apart from a truncated value.
///
/// Deliberately not `Copy`, unlike the value types beside it: the last variant
/// carries a [`ParseIntError`], and `Copy` is only worth having on a type that
/// lives on the request path. This one is produced once, on a refusal.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum McpSessionIdError {
    /// The value was nothing but optional whitespace.
    #[error("Mcp-Session-Id is empty once surrounding whitespace is trimmed")]
    Empty,

    /// A byte outside the visible-ASCII range reached the value.
    #[error(
        "Mcp-Session-Id contains {byte:#04X} at byte {index}, outside the \
         visible ASCII range {low:#04X}-{high:#04X} required of an HTTP field-value"
    )]
    NotVisibleAscii {
        /// Byte offset within the trimmed value.
        index: usize,
        /// The offending byte.
        byte: u8,
        /// Lowest permitted byte, [`VISIBLE_ASCII_RANGE`]'s start.
        low: u8,
        /// Highest permitted byte, [`VISIBLE_ASCII_RANGE`]'s end.
        high: u8,
    },

    /// The value was not sixteen characters.
    #[error("Mcp-Session-Id must be {expected} hexadecimal digits, found {found}")]
    WrongLength {
        /// The fixed width, [`DIGITS`].
        expected: usize,
        /// What arrived.
        found: usize,
    },

    /// A character in the value was not a hexadecimal digit.
    #[error(
        "Mcp-Session-Id contains {character:?} at byte {index}, which is not a hexadecimal digit"
    )]
    NotHex {
        /// Byte offset within the trimmed value.
        index: usize,
        /// The offending character.
        character: char,
    },

    /// The digits were well-formed but did not denote a `u64`.
    #[error("Mcp-Session-Id is not a valid hexadecimal number: {source}")]
    Unparsable {
        /// The underlying conversion failure.
        source: ParseIntError,
    },
}

impl McpSessionIdError {
    /// Checks a raw header value and returns its significant part.
    ///
    /// Trimming first is not leniency for its own sake: RFC 9110 §5.5 allows
    /// optional whitespace around a field-value and an HTTP parser is entitled
    /// to hand the field with it still attached. Rejecting a legal header
    /// because of transport framing the client could not have avoided would be
    /// our bug, not theirs.
    ///
    /// Interior whitespace is *not* tolerated even though `field-value`
    /// technically permits it, and the narrowing is deliberate: that allowance
    /// is for a human-typed `Subject:`, and a session id is a machine-carried
    /// identifier that no reader would expect to be trimmed. A value with a tab
    /// in the middle is refused as not visible ASCII, which is where a reader
    /// would look for it.
    ///
    /// # Errors
    ///
    /// [`McpSessionIdError::Empty`] for a wholly blank value, and
    /// [`McpSessionIdError::NotVisibleAscii`] naming the first byte that is out
    /// of range. Only the *first* offender is reported: the value is a header,
    /// and a message listing every bad byte in a 4 KiB garbage string is noise.
    pub fn validate(value: &str) -> Result<&str, Self> {
        let trimmed = value.trim_matches([' ', '\t']);
        if trimmed.is_empty() {
            return Err(Self::Empty);
        }
        // The range check runs first, ahead of the width check. A value that
        // carries a character outside visible ASCII is not a session id of the
        // wrong length, it is not a session id at all, and reporting its width
        // would describe a problem the caller does not have.
        //
        // Walked by `char_indices` rather than by `bytes` so the index reported
        // is where the character *starts* and the value reported is the
        // character itself. A byte walk would name the UTF-8 lead byte of
        // `é` (0xC3) rather than `é` (0xE9), and the caller would be left
        // decoding the difference.
        for (index, character) in trimmed.char_indices() {
            let byte = u8::try_from(character).unwrap_or(u8::MAX);
            if !VISIBLE_ASCII_RANGE.contains(&byte) {
                return Err(Self::NotVisibleAscii {
                    index,
                    byte,
                    low: *VISIBLE_ASCII_RANGE.start(),
                    high: *VISIBLE_ASCII_RANGE.end(),
                });
            }
        }
        // Width is measured in **bytes**, because that is what an HTTP field
        // carries and what the header is written as. Counting characters
        // instead would let a multi-byte character pad a short value up to the
        // right width, which is a value nothing can round-trip.
        if trimmed.len() != DIGITS {
            return Err(Self::WrongLength {
                expected: DIGITS,
                found: trimmed.len(),
            });
        }
        for (index, character) in trimmed.char_indices() {
            if !character.is_ascii_hexdigit() {
                return Err(Self::NotHex { index, character });
            }
        }
        Ok(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::{DIGITS, McpSessionId, McpSessionIdError, VISIBLE_ASCII_RANGE};
    use crate::domain::session::SessionId;
    use core::str::FromStr;

    /// Every failure below returns `Result` and every call is `?`'d, so a
    /// broken fixture reports itself rather than needing an `.unwrap()` to
    /// surface as a panic.
    #[derive(Debug)]
    enum Failure {
        Session(McpSessionIdError),
        Display,
    }

    impl From<McpSessionIdError> for Failure {
        fn from(error: McpSessionIdError) -> Self {
            Self::Session(error)
        }
    }

    /// `?` carries the concrete refusal rather than flattening it, so a test
    /// that expected a parse to fail still says _why_ it did.
    #[test]
    fn a_conversion_keeps_the_refusal_it_was_given() -> Result<(), Failure> {
        let failure: Failure = McpSessionIdError::Empty.into();
        let Failure::Session(reason) = failure else {
            return Err(Failure::Display);
        };
        if reason.to_string().is_empty() {
            return Err(Failure::Display);
        }
        Ok(())
    }

    /// The wire form of a freshly minted id is exactly the fixed width and is
    /// visible ASCII byte for byte, for the whole `u64` range.
    ///
    /// The width claim is the load-bearing one: a shorter-than-16 rendering
    /// would still round-trip, so only this test catches it.
    #[test]
    fn rendering_is_fixed_width_visible_ascii() -> Result<(), Failure> {
        // A handful of values chosen to cover the boundaries of a u64 plus the
        // digits that differ most in their high bit.
        for value in [0, 1, 9, 10, 15, 16, 255, 256, u32::MAX.into(), u64::MAX] {
            let id = McpSessionId::new(SessionId::new(value));
            let rendered = id.to_string();
            if rendered.len() != DIGITS {
                return Err(Failure::Display);
            }
            if !rendered.bytes().all(|b| VISIBLE_ASCII_RANGE.contains(&b)) {
                return Err(Failure::Display);
            }
        }
        Ok(())
    }

    /// The known encodings, spelled out, so a change to the format is a
    /// deliberate edit here rather than a silent one.
    #[test]
    fn renders_the_documented_spelling() -> Result<(), Failure> {
        for (session, expected) in [
            (0, "0000000000000000"),
            (1, "0000000000000001"),
            (255, "00000000000000ff"),
        ] {
            if McpSessionId::new(SessionId::new(session)).to_string() != expected {
                return Err(Failure::Display);
            }
        }
        Ok(())
    }

    /// Render, parse, and land back on the same session — the bijection the
    /// "derive, do not mint a second id" decision rests on.
    #[test]
    fn round_trips_through_the_header_form() -> Result<(), Failure> {
        for value in [0, 1, 7, 123_456_789, u64::MAX] {
            let original = McpSessionId::new(SessionId::new(value));
            let parsed = McpSessionId::from_str(&original.to_string())?;
            if parsed != original {
                return Err(Failure::Display);
            }
            if parsed.session() != SessionId::new(value) {
                return Err(Failure::Display);
            }
        }
        Ok(())
    }

    /// Lowercase is accepted and normalises to the canonical uppercase spelling,
    /// because the id is a value and a client is not required to preserve its
    /// case to be understood.
    #[test]
    fn accepts_lowercase_and_agrees_with_uppercase() -> Result<(), Failure> {
        if McpSessionId::from_str("00000000000000ff")?
            != McpSessionId::from_str("00000000000000FF")?
        {
            return Err(Failure::Display);
        }
        Ok(())
    }

    /// Surrounding whitespace is transport framing and is trimmed rather than
    /// refused, per RFC 9110 §5.5's optional whitespace.
    #[test]
    fn trims_surrounding_whitespace() -> Result<(), Failure> {
        if McpSessionId::from_str("  0000000000000001\t")?.session() != SessionId::new(1) {
            return Err(Failure::Display);
        }
        Ok(())
    }

    /// A byte outside `%x20-7E` is refused, and the refusal names the byte and
    /// where it was. This is the header-grammar rule, distinct from the format
    /// rule: the value below is also the right length and fails earlier.
    #[test]
    fn refuses_non_visible_ascii_naming_the_byte() {
        let refused = McpSessionId::from_str("000000000000000\n");
        assert_eq!(
            refused,
            Err(McpSessionIdError::NotVisibleAscii {
                index: 15,
                byte: b'\n',
                low: *VISIBLE_ASCII_RANGE.start(),
                high: *VISIBLE_ASCII_RANGE.end(),
            })
        );
    }

    /// A byte in the obs-text range — legal in a `field-value` per RFC 9110 and
    /// still refused, because the encoding has no non-ASCII spelling.
    #[test]
    fn refuses_obs_text() {
        assert_eq!(
            McpSessionId::from_str("00000000000000\u{00ff}"),
            Err(McpSessionIdError::NotVisibleAscii {
                index: 14,
                byte: 0xff,
                low: *VISIBLE_ASCII_RANGE.start(),
                high: *VISIBLE_ASCII_RANGE.end(),
            })
        );
    }

    /// Blank, wrong-width, and non-hex are three different mistakes and come
    /// back as three different variants, so a caller can tell a truncated value
    /// from a mangled one.
    #[test]
    fn distinguishes_empty_wrong_length_and_non_hex() {
        assert_eq!(McpSessionId::from_str("   "), Err(McpSessionIdError::Empty));
        assert_eq!(McpSessionId::from_str(""), Err(McpSessionIdError::Empty));
        assert_eq!(
            McpSessionId::from_str("0001"),
            Err(McpSessionIdError::WrongLength {
                expected: DIGITS,
                found: 4,
            })
        );
        assert_eq!(
            McpSessionId::from_str("zzzzzzzzzzzzzzzz"),
            Err(McpSessionIdError::NotHex {
                index: 0,
                character: 'z',
            })
        );
    }

    /// A character above ASCII is caught by the range check before it can be
    /// named as a `char`, so `NotHex` never has to represent a non-ASCII
    /// character.
    #[test]
    fn range_check_runs_before_the_digit_check() {
        assert_eq!(
            McpSessionId::from_str(
                "\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}"
            ),
            Err(McpSessionIdError::NotVisibleAscii {
                index: 0,
                byte: 0xe9,
                low: *VISIBLE_ASCII_RANGE.start(),
                high: *VISIBLE_ASCII_RANGE.end(),
            })
        );
    }

    /// An id is an ordinary `u64` underneath, so the conversions between the
    /// two identities are the identity. Checked directly so a future
    /// non-`Copy` field cannot quietly change the answer.
    #[test]
    fn converts_to_and_from_the_internal_id() -> Result<(), Failure> {
        let internal = SessionId::new(42);
        let wire: McpSessionId = internal.into();
        if wire.session() != internal {
            return Err(Failure::Display);
        }
        let back: SessionId = wire.into();
        if back != internal {
            return Err(Failure::Display);
        }
        Ok(())
    }

    /// `as_bytes` and `Display` are two renderings of one format and must not
    /// be allowed to drift.
    #[test]
    fn bytes_and_display_agree() -> Result<(), Failure> {
        let id = McpSessionId::new(SessionId::new(0xdead_beef));
        let mut buffer = [0u8; DIGITS];
        if id.as_bytes(&mut buffer).as_slice() != id.to_string().into_bytes() {
            return Err(Failure::Display);
        }
        Ok(())
    }
}
