//! `Mcp-Method` and `Mcp-Name` — the standard routing headers.
//!
//! # The spelling, and where it came from
//!
//! `crates/protocol-droid-schema/spec/2026-07-28/CHANGELOG.md`, **Minor changes
//! #4**: "Require standard MCP request headers (`Mcp-Method`, `Mcp-Name`) on
//! Streamable HTTP POST requests, and add support for custom headers from tool
//! parameters via `x-mcp-header` (SEP-2243)."
//!
//! Three facts follow from that sentence and each one shaped a decision here.
//!
//! **The headers are required, not optional, from `2026-07-28`.** So the gate
//! is on whether the *negotiated profile* carries
//! [`Feature::StandardRoutingHeaders`], which the feature matrix dates to
//! `2026-07-28` — nothing in the `2025-11-25` or earlier changelogs mentions
//! them, and `schema.json` for `2026-07-28` does not define them either. They
//! are a transport concern, which is why they have no entry in the JSON schema
//! and why [`RoutingHeaders::for_profile`] refuses to build a pair the peer
//! was never offered.
//!
//! **They are required only on POST.** Not on the stream, not on a response. So
//! this module has no opinion about a response's headers and a caller that wants
//! to emit one has to add it; the type models the request side only.
//!
//! **They are request routing, not request content.** `Mcp-Method` carries the
//! JSON-RPC method name — a value that already exists in the IR as
//! [`MethodName`] — and `Mcp-Name` carries the name of the thing the method acts
//! on. The two are validated together, because a `Mcp-Method` without a
//! `Mcp-Name` addresses nothing and a `Mcp-Name` without a `Mcp-Method` is not
//! attached to an operation.
//!
//! # On `Mcp-Param-*`
//!
//! There is no such header. It appears in no vendored revision, and the
//! mechanism the specification actually describes for carrying a tool
//! parameter into a header is the `x-mcp-header` JSON Schema keyword on a
//! property schema — see the `inputSchema` description in
//! `crates/protocol-droid-schema/spec/2026-07-28/schema.json`, which says a
//! property schema "may carry an `x-mcp-header` annotation to mirror the
//! argument value into an HTTP header on the Streamable HTTP transport". The
//! extraction rules it refers to live in the transport specification, which this
//! repository does not vendor, so there is nothing here to implement against
//! and nothing is invented. The prefix stays out of the code until the rules
//! can be read rather than guessed.
//!
//! # Why the values are typed and not two `String`s
//!
//! Both values reach a server before a body is parsed, and both decide what the
//! body is going to be dispatched as. A `String` at that point is exactly the
//! "validate, don't parse" failure the project rules name: the handler would
//! re-derive the method name and re-check the shape on the way in. Constructing
//! a [`RoutingTarget`] performs those checks once, and the header parse
//! produces one directly.
//!
//! # Why the header *name* is compared case-insensitively
//!
//! Because RFC 9110 §5.1 makes field names case-insensitive, and `mcp-method`
//! from a client is the same header as `Mcp-Method`. The *values* stay
//! case-sensitive: a method name and a tool name are both case-sensitive in the
//! IR, and lowercasing one to compare it would be a silent data change.

use core::fmt;
use std::sync::Arc;

use thiserror::Error;

use crate::domain::Feature;
use crate::domain::feature::FeatureProfile;
use crate::domain::ir::MethodName;
use crate::domain::mcp::session_id::VISIBLE_ASCII_RANGE;

/// The method header's name, exactly as the specification spells it.
pub const MCP_METHOD_HEADER: &str = "Mcp-Method";

/// The target-name header's name, exactly as the specification spells it.
pub const MCP_NAME_HEADER: &str = "Mcp-Name";

/// The name of a tool, resource or prompt.
///
/// Distinct from [`MethodName`] because the two are constrained differently: a
/// method name is slash-separated and its shape is a JSON-RPC convention, while
/// a target name is whatever the server chose to call the thing and is only
/// required to survive a trip through an HTTP field-value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoutingName(Arc<str>);

impl RoutingName {
    /// A target name.
    ///
    /// # Errors
    ///
    /// [`RoutingHeaderError::EmptyName`] for a blank name, and
    /// [`RoutingHeaderError::NonVisibleAscii`] naming the first byte outside
    /// [`VISIBLE_ASCII_RANGE`].
    pub fn new(name: &str) -> Result<Self, RoutingHeaderError> {
        if name.is_empty() {
            return Err(RoutingHeaderError::EmptyName);
        }
        if let Some((index, byte)) = name
            .bytes()
            .enumerate()
            .find(|(_, byte)| !VISIBLE_ASCII_RANGE.contains(byte))
        {
            return Err(RoutingHeaderError::NonVisibleAscii { index, byte });
        }
        Ok(Self(Arc::from(name)))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoutingName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The operation a request addresses, and the thing it addresses it against.
///
/// A pair rather than two loose strings because the specification requires the
/// two together: a `Mcp-Method` on its own routes to an operation but names no
/// target, and a tool call is the only method that is about one specific
/// thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingTarget {
    method: MethodName,
    name: RoutingName,
}

impl RoutingTarget {
    /// Pairs a method with the thing it acts on.
    #[must_use]
    pub const fn new(method: MethodName, name: RoutingName) -> Self {
        Self { method, name }
    }

    /// The operation.
    #[must_use]
    pub const fn method(&self) -> &MethodName {
        &self.method
    }

    /// The thing being operated on.
    #[must_use]
    pub const fn name(&self) -> &RoutingName {
        &self.name
    }
}

/// A validated `Mcp-Method`/`Mcp-Name` pair, proven admissible for one session.
///
/// The profile is not stored. It is checked once, at construction, and the fact
/// that construction succeeded *is* the proof — a type that could be built
/// without a profile would be a way to skip the gate, and a type that carried
/// the profile would let a later caller re-check a stale answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingHeaders(RoutingTarget);

impl RoutingHeaders {
    /// Builds the pair, but only for a session that was offered the feature.
    ///
    /// # Errors
    ///
    /// [`RoutingHeaderError::FeatureUnavailable`] when the negotiated profile
    /// does not carry [`Feature::StandardRoutingHeaders`] — that is, for every
    /// revision before `2026-07-28`, and for a `2026-07-28` session whose
    /// profile was resolved without it.
    pub fn for_profile(
        target: RoutingTarget,
        profile: FeatureProfile,
    ) -> Result<Self, RoutingHeaderError> {
        if !profile.allows(Feature::StandardRoutingHeaders) {
            return Err(RoutingHeaderError::FeatureUnavailable {
                feature: Feature::StandardRoutingHeaders,
            });
        }
        Ok(Self(target))
    }

    /// Parses the pair out of a request's header fields.
    ///
    /// `fields` is whatever the HTTP layer holds, in any case, in any order.
    /// Names are matched case-insensitively; values are not, because both values
    /// are case-sensitive in the IR.
    ///
    /// # Errors
    ///
    /// [`RoutingHeaderError::FeatureUnavailable`] when the profile does not
    /// carry [`Feature::StandardRoutingHeaders`] — checked *before* the fields
    /// are even read, so a pre-`2026-07-28` client is refused for the right
    /// reason rather than for a missing header. Then
    /// [`RoutingHeaderError::Missing`] naming whichever of the two is absent,
    /// [`RoutingHeaderError::Repeated`] for a header sent twice, and the
    /// value errors from [`MethodName::new`] and [`RoutingName::new`] for a
    /// present but malformed pair.
    pub fn parse<'a, I>(fields: I, profile: FeatureProfile) -> Result<Self, RoutingHeaderError>
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        if !profile.allows(Feature::StandardRoutingHeaders) {
            return Err(RoutingHeaderError::FeatureUnavailable {
                feature: Feature::StandardRoutingHeaders,
            });
        }
        let mut method: Option<&str> = None;
        let mut name: Option<&str> = None;
        for (field, value) in fields {
            if field.eq_ignore_ascii_case(MCP_METHOD_HEADER) {
                set_once(&mut method, value)?;
            } else if field.eq_ignore_ascii_case(MCP_NAME_HEADER) {
                set_once(&mut name, value)?;
            }
        }
        let method = method.ok_or(RoutingHeaderError::Missing {
            header: MCP_METHOD_HEADER,
        })?;
        let name = name.ok_or(RoutingHeaderError::Missing {
            header: MCP_NAME_HEADER,
        })?;
        Ok(Self(RoutingTarget::new(
            MethodName::new(method)?,
            RoutingName::new(name)?,
        )))
    }

    /// What was addressed.
    #[must_use]
    pub const fn target(&self) -> &RoutingTarget {
        &self.0
    }

    /// The header fields to put on the request, in the order they should go out.
    ///
    /// One serialiser, returned as an iterator of borrowed pairs, so the adapter
    /// writes them into whatever header container it has without a second copy
    /// of the spelling existing anywhere else.
    pub fn fields(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            (MCP_METHOD_HEADER, self.0.method.as_str()),
            (MCP_NAME_HEADER, self.0.name.as_str()),
        ]
        .into_iter()
    }
}

impl fmt::Display for RoutingHeaders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (name, value) in self.fields() {
            if !first {
                formatter.write_str("; ")?;
            }
            write!(formatter, "{name}: {value}")?;
            first = false;
        }
        Ok(())
    }
}

/// Records a value, refusing a second one.
///
/// A repeated routing header is a refusal rather than a last-one-wins because
/// the two values disagree about which operation is being called, and silently
/// preferring one of them is a routing decision made by an attacker.
const fn set_once<'a>(
    slot: &mut Option<&'a str>,
    value: &'a str,
) -> Result<(), RoutingHeaderError> {
    if slot.is_some() {
        return Err(RoutingHeaderError::Repeated);
    }
    *slot = Some(value);
    Ok(())
}

/// Why a request is not routable by header.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RoutingHeaderError {
    /// The negotiated session was never offered the standard routing headers.
    #[error("the negotiated profile does not include the {feature} feature")]
    FeatureUnavailable {
        /// The feature that was missing.
        feature: Feature,
    },

    /// One of the two required headers was absent.
    #[error("the required {header} header is missing")]
    Missing {
        /// The header that was absent.
        header: &'static str,
    },

    /// A header arrived more than once.
    #[error("a routing header was supplied more than once, and the two values disagree")]
    Repeated,

    /// The target name was blank.
    #[error("Mcp-Name is empty")]
    EmptyName,

    /// A byte outside the visible-ASCII range reached a header value.
    #[error(
        "a routing header contains {byte:#04X} at byte {index}, which cannot be sent as an HTTP field-value"
    )]
    NonVisibleAscii {
        /// Byte offset within the value.
        index: usize,
        /// The offending byte.
        byte: u8,
    },

    /// The method name was not a well-formed JSON-RPC method.
    #[error("Mcp-Method is not a well-formed method name: {0}")]
    Method(#[from] crate::domain::ir::MethodNameError),
}

#[cfg(test)]
mod tests {
    use super::{
        MCP_METHOD_HEADER, MCP_NAME_HEADER, RoutingHeaderError, RoutingHeaders, RoutingName,
        RoutingTarget,
    };
    use crate::domain::Feature;
    use crate::domain::feature::FeatureProfile;
    use crate::domain::ir::MethodName;

    /// A profile that carries the feature, standing in for a `2026-07-28`
    /// session. Built through the matrix rather than hand-set so the test fails
    /// if the matrix row moves.
    fn negotiating_profile() -> FeatureProfile {
        FeatureProfile::empty().with(Feature::StandardRoutingHeaders)
    }

    #[derive(Debug)]
    enum Failure {
        Routing(RoutingHeaderError),
        Method(crate::domain::ir::MethodNameError),
        /// A test's own assertion failed, with nothing for the parser to have
        /// said about it.
        Display,
    }

    impl From<RoutingHeaderError> for Failure {
        fn from(error: RoutingHeaderError) -> Self {
            Self::Routing(error)
        }
    }

    impl From<crate::domain::ir::MethodNameError> for Failure {
        fn from(error: crate::domain::ir::MethodNameError) -> Self {
            Self::Method(error)
        }
    }

    fn target() -> Result<RoutingTarget, Failure> {
        Ok(RoutingTarget::new(
            MethodName::new("tools/call")?,
            RoutingName::new("weather")?,
        ))
    }

    /// A well-formed pair on a session that has the feature parses, and lands on
    /// the operation and the target the caller expected.
    #[test]
    fn parses_a_well_formed_pair() -> Result<(), Failure> {
        let fields = [
            (MCP_METHOD_HEADER, "tools/call"),
            (MCP_NAME_HEADER, "weather"),
        ];
        let parsed = RoutingHeaders::parse(fields, negotiating_profile())?;
        if parsed.target().method().as_str() != "tools/call" {
            return Err(Failure::Routing(RoutingHeaderError::Missing {
                header: MCP_METHOD_HEADER,
            }));
        }
        if parsed.target().name().as_str() != "weather" {
            return Err(Failure::Routing(RoutingHeaderError::EmptyName));
        }
        Ok(())
    }

    /// The spelling is the single place the header names are written. If the
    /// specification's spelling ever changes, this is the test that says so
    /// before an adapter does.
    #[test]
    fn emits_the_spelling_the_specification_fixes() -> Result<(), Failure> {
        let headers = RoutingHeaders::for_profile(target()?, negotiating_profile())?;
        let emitted: Vec<(&str, &str)> = headers.fields().collect();
        let expected = vec![("Mcp-Method", "tools/call"), ("Mcp-Name", "weather")];
        if emitted != expected {
            return Err(Failure::Routing(RoutingHeaderError::Missing {
                header: MCP_METHOD_HEADER,
            }));
        }
        Ok(())
    }

    /// The pair emitted is exactly the pair that parses back, so a client using
    /// our own writer is understood by our own reader.
    #[test]
    fn emitted_fields_reparse() -> Result<(), Failure> {
        let headers = RoutingHeaders::for_profile(target()?, negotiating_profile())?;
        let wire: Vec<(String, String)> = headers
            .fields()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        let borrowed: Vec<(&str, &str)> = wire
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        if RoutingHeaders::parse(borrowed, negotiating_profile())? != headers {
            return Err(Failure::Routing(RoutingHeaderError::Repeated));
        }
        Ok(())
    }

    /// `?` carries the concrete refusal rather than flattening it to a string.
    ///
    /// The tests above all convert through `?`, so if the conversion dropped
    /// the error, every one of them would report the same uninformative
    /// failure. This reads the payload back so the two variants stay distinct
    /// and the error survives the trip.
    #[test]
    fn a_conversion_keeps_the_refusal_it_was_given() -> Result<(), Failure> {
        let routing: Failure = RoutingHeaderError::Repeated.into();
        let method: Failure = crate::domain::ir::MethodNameError::Empty.into();
        let (Failure::Routing(repeated), Failure::Method(empty)) = (routing, method) else {
            return Err(Failure::Display);
        };
        if repeated.to_string().is_empty() || empty.to_string().is_empty() {
            return Err(Failure::Display);
        }
        Ok(())
    }

    /// Field names are case-insensitive per RFC 9110 §5.1, and a client that
    /// lowercased them means the same request.
    #[test]
    fn accepts_header_names_in_any_case() -> Result<(), Failure> {
        let fields = [("mcp-method", "tools/call"), ("MCP-NAME", "weather")];
        let parsed = RoutingHeaders::parse(fields, negotiating_profile())?;
        if parsed.target().name().as_str() != "weather" {
            return Err(Failure::Routing(RoutingHeaderError::EmptyName));
        }
        Ok(())
    }

    /// Values stay case-sensitive. A method name lowercased by an intermediary
    /// is a different method, and treating it as one would dispatch to the wrong
    /// place rather than failing.
    #[test]
    fn does_not_fold_value_case() {
        let fields = [
            (MCP_METHOD_HEADER, "Tools/Call"),
            (MCP_NAME_HEADER, "weather"),
        ];
        assert_eq!(
            RoutingHeaders::parse(fields, FeatureProfile::all()).map(|h| h
                .target()
                .method()
                .as_str()
                .to_owned()),
            Ok("Tools/Call".to_owned())
        );
    }

    /// The gate is the reason this type exists, so it is checked on both
    /// directions: building, and parsing.
    #[test]
    fn refuses_when_the_profile_lacks_the_feature() -> Result<(), Failure> {
        let without = FeatureProfile::empty();
        let expected = RoutingHeaderError::FeatureUnavailable {
            feature: Feature::StandardRoutingHeaders,
        };
        if RoutingHeaders::for_profile(target()?, without) != Err(expected.clone()) {
            return Err(Failure::Routing(expected));
        }
        // Even a perfectly formed pair is refused on a session that was never
        // offered the feature, and the refusal is about the feature rather than
        // about the headers.
        let fields = [
            (MCP_METHOD_HEADER, "tools/call"),
            (MCP_NAME_HEADER, "weather"),
        ];
        if RoutingHeaders::parse(fields, without) != Err(expected.clone()) {
            return Err(Failure::Routing(expected));
        }
        Ok(())
    }

    /// Each missing header is named, so a client that sent one of the two can
    /// tell which to add.
    #[test]
    fn names_the_missing_header() {
        let profile = FeatureProfile::all();
        assert_eq!(
            RoutingHeaders::parse([(MCP_METHOD_HEADER, "tools/call")], profile),
            Err(RoutingHeaderError::Missing {
                header: MCP_NAME_HEADER
            })
        );
        assert_eq!(
            RoutingHeaders::parse([(MCP_NAME_HEADER, "weather")], profile),
            Err(RoutingHeaderError::Missing {
                header: MCP_METHOD_HEADER
            })
        );
    }

    /// A repeated header is a refusal, not last-one-wins.
    #[test]
    fn refuses_a_repeated_header() {
        let fields = [
            (MCP_METHOD_HEADER, "tools/call"),
            (MCP_METHOD_HEADER, "resources/read"),
            (MCP_NAME_HEADER, "weather"),
        ];
        assert_eq!(
            RoutingHeaders::parse(fields, FeatureProfile::all()),
            Err(RoutingHeaderError::Repeated)
        );
    }

    /// A value that cannot be a header field-value is refused at the point it is
    /// parsed, naming the byte — before it can reach an adapter that would
    /// discover it by failing to write.
    #[test]
    fn refuses_a_non_visible_ascii_value() {
        let fields = [
            (MCP_METHOD_HEADER, "tools/call"),
            (MCP_NAME_HEADER, "wea\nther"),
        ];
        assert_eq!(
            RoutingHeaders::parse(fields, FeatureProfile::all()),
            Err(RoutingHeaderError::NonVisibleAscii {
                index: 3,
                byte: b'\n'
            })
        );
    }

    /// A malformed method name is refused through the IR's own error, so the
    /// header path and the body path cannot disagree about what a method name
    /// is.
    #[test]
    fn defers_method_validation_to_the_ir() {
        let fields = [(MCP_METHOD_HEADER, "tools/"), (MCP_NAME_HEADER, "weather")];
        let refused = RoutingHeaders::parse(fields, FeatureProfile::all());
        assert!(
            matches!(refused, Err(RoutingHeaderError::Method(_))),
            "{refused:?}"
        );
    }

    /// A blank target name is refused. It is the one value check that is not
    /// delegated, because `MethodName` has no counterpart to defer to.
    #[test]
    fn refuses_a_blank_target_name() {
        assert_eq!(RoutingName::new(""), Err(RoutingHeaderError::EmptyName));
    }
}
