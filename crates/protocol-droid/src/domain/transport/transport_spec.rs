//! How a transport was named on a command line, as a value.
//!
//! The CLI's job is to turn `--upstream stdio:./server` into something the
//! bridge can act on. `clap` can validate the *shape* of a string, but what
//! arrives on this side is still text, and text is exactly the thing the
//! project rules forbid from crossing into shared code. So the adapter parses
//! it here, once, into this type, and everything downstream sees a
//! [`TransportKind`] and a target it does not have to re-interpret.
//!
//! ## The grammar
//!
//! ```text
//!   spec   := kind [ ":" target ]
//!   kind   := "stdio" | "sse" | "http" | "http-stateless" | "streamable"
//!   target := any non-empty, non-whitespace run
//! ```
//!
//! Two decisions worth stating:
//!
//! **A missing target is an error for `stdio` and a default for the rest.**
//! `stdio` *is* a target — the process on the other end — so `stdio` alone
//! names nothing to connect to. `http://…` has an obvious default and making
//! the user type it would be a papercut. Guessing the wrong way round fails
//! late and confusingly, so the asymmetry is deliberate.
//!
//! **`sse` and `streamable` are distinct kinds, not synonyms.** They are
//! different revisions' transports with different framing, different session
//! semantics and different headers. Accepting one name for both would make a
//! bridge silently pick a transport the user did not ask for, which is the
//! worst thing a proxy can do.

use core::fmt;
use core::str::FromStr;
use std::sync::Arc;

use crate::domain::TransportKind;

/// A transport named at an adapter boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportSpec {
    /// Which transport this names.
    kind: TransportKind,
    /// Where it goes. Never empty.
    target: Arc<str>,
}

impl TransportSpec {
    /// A spec for `kind` pointed at `target`.
    ///
    /// # Errors
    ///
    /// [`SpecError::EmptyTarget`] when `target` is empty. A spec with no
    /// target is a spec that cannot be acted on, and letting it exist means
    /// the failure surfaces later as a connection to nothing.
    pub fn new(kind: TransportKind, target: impl Into<Arc<str>>) -> Result<Self, SpecError> {
        let target = target.into();
        if target.is_empty() {
            return Err(SpecError::EmptyTarget);
        }
        Ok(Self { kind, target })
    }

    /// The transport named.
    #[must_use]
    pub const fn kind(&self) -> TransportKind {
        self.kind
    }

    /// Where it goes.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }
}

/// Why a transport spec could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    /// The part before the `:` is not a transport this build knows.
    #[error("{name:?} is not a transport this build speaks; expected one of {expected}")]
    UnknownTransport {
        /// What the caller wrote.
        name: String,
        /// What would have been accepted, in order.
        expected: &'static str,
    },

    /// Nothing at all.
    #[error("expected a transport, found nothing")]
    Empty,

    /// The part after the `:` was empty.
    ///
    /// Distinct from [`SpecError::Empty`] because `stdio:` is a caller who
    /// clearly meant to name a target and did not, which is a different
    /// mistake from naming no transport.
    #[error("expected a target after the ':', found nothing")]
    EmptyTarget,

    /// A transport that needs a target was named without one.
    #[error("{kind} names a process or a socket, so a target is required")]
    MissingTarget {
        /// The transport that was named.
        kind: &'static str,
    },
}

impl FromStr for TransportSpec {
    type Err = SpecError;

    /// Parses `kind[:target]`.
    ///
    /// # Errors
    ///
    /// Any [`SpecError`]: an unrecognised transport, nothing at all, or a
    /// transport named without the target it needs.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(SpecError::Empty);
        }

        let (name, target) = match trimmed.split_once(':') {
            Some((name, target)) => (name, Some(target)),
            None => (trimmed, None),
        };

        let kind = parse_kind(name)?;

        // A remainder beginning `//` is the user writing a URL, not a target
        // that happens to start with two slashes. `split_once` above split on
        // the *scheme's* colon, so the scheme has to go back on — otherwise
        // `http://host:3000/mcp` yields `//host:3000/mcp`, which is a
        // protocol-relative URL, and every adapter that tries to build a
        // request against it fails on a string the caller never wrote.
        //
        // This is deliberately keyed on `//` and not on the kind, so
        // `sse:http://old.example/sse` — where the remainder already carries a
        // full scheme and is left alone — still parses exactly as before.
        let target = target.map(|rest| match rest.starts_with("//") {
            true => format!("{name}:{rest}"),
            false => rest.to_owned(),
        });

        match target.as_deref() {
            Some(target) if !target.is_empty() => Self::new(kind, target),
            Some(_) => Err(SpecError::EmptyTarget),
            None if is_process(name) => Err(SpecError::MissingTarget {
                kind: process_name(name).unwrap_or("stdio"),
            }),
            None => Self::new(kind, DEFAULT_TARGET_FOR_HTTP),
        }
    }
}

impl fmt::Display for TransportSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = self.kind.as_str();
        // A URL-shaped target already carries its own scheme, so writing
        // `kind:` in front of it produces `http:http://…`. The spec is the
        // thing a user copies into a shell, so it has to read back as the one
        // line that parses to this same spec.
        match self.target.starts_with(&format!("{kind}:")) {
            true => formatter.write_str(&self.target),
            false => write!(formatter, "{kind}:{}", self.target),
        }
    }
}

/// The transport names accepted on a command line, in the order they are
/// offered in an error message.
const ACCEPTED: &str = "stdio, sse, http, http-stateless (alias: streamable)";

fn parse_kind(name: &str) -> Result<TransportKind, SpecError> {
    // The spellings are `TransportKind::as_str`, so the CLI and the error
    // messages cannot drift from each other. `streamable` is accepted because
    // it is what everyone actually types, and it is the *only* alias: the
    // other three are short enough that a second name for each would be three
    // more things to keep in sync for no gain.
    match name {
        "streamable" => Ok(TransportKind::StreamableHttp),
        other => TransportKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == other)
            .ok_or_else(|| SpecError::UnknownTransport {
                name: other.to_owned(),
                expected: ACCEPTED,
            }),
    }
}

/// Whether this transport cannot be named without saying where to go.
///
/// Derived from the spelling rather than from [`TransportKind`] because the
/// caller named `http`, not `StreamableHttp` — the error has to quote back
/// something they typed.
fn is_process(name: &str) -> bool {
    name == "stdio" || name == "sse"
}

/// The static spelling of a transport that names a process.
///
/// `MissingTarget` quotes back something the caller typed, and the error
/// outlives the borrow it was built from, so it has to name a `&'static str`
/// rather than the slice it was parsed out of.
fn process_name(name: &str) -> Option<&'static str> {
    match name {
        "stdio" => Some("stdio"),
        "sse" => Some("sse"),
        _ => None,
    }
}

/// Where an HTTP transport points when the caller did not say.
const DEFAULT_TARGET_FOR_HTTP: &str = "http://127.0.0.1:3000/mcp";

#[cfg(test)]
mod tests {
    use core::str::FromStr;

    use crate::domain::TransportKind;

    use super::{SpecError, TransportSpec};

    type TestError = Box<dyn std::error::Error>;

    /// Names the [`SpecError`] variant an input must be refused as.
    ///
    /// A function pointer rather than an expected `SpecError`, because the
    /// cases are built as an array and constructing them all would need the
    /// error values anyway — which is the thing under test.
    type Expected = fn(&SpecError) -> bool;

    /// The ordinary cases, each asserting both halves of the result — a parse
    /// that got the kind right and the target wrong is not a successful parse.
    #[test]
    fn a_fully_qualified_spec_parses() -> Result<(), TestError> {
        let cases: [(&str, TransportKind, &str); 4] = [
            ("stdio:./server", TransportKind::Stdio, "./server"),
            (
                "http://srv.example/mcp",
                TransportKind::StreamableHttp,
                "http://srv.example/mcp",
            ),
            (
                "sse:http://old.example/sse",
                TransportKind::LegacySse,
                "http://old.example/sse",
            ),
            (
                "http-stateless:http://new.example/mcp",
                TransportKind::StatelessHttp,
                "http://new.example/mcp",
            ),
        ];

        for (input, kind, target) in cases {
            let spec = TransportSpec::from_str(input)?;
            assert_eq!(spec.kind(), kind, "{input} named the wrong transport");
            assert_eq!(spec.target(), target, "{input} took the wrong target");
        }
        Ok(())
    }

    /// `http` and `streamable` are two spellings of one transport, and the
    /// target split on the *first* colon keeps a URL intact.
    /// The port colon in a URL is part of the target, not another separator.
    ///
    /// This used to assert the target was `//example.com:8443/mcp`, which kept the
    /// port and dropped the scheme. The port is the half of that claim that was
    /// worth making; the scheme is what made the result unusable, and an adapter
    /// building a request against it failed on a string the caller never wrote. The
    /// caller wrote `http://example.com:8443/mcp`, and that is what is kept.
    #[test]
    fn a_url_target_keeps_its_own_colons() -> Result<(), TestError> {
        let spec = TransportSpec::from_str("http://example.com:8443/mcp")?;

        assert_eq!(spec.kind(), TransportKind::StreamableHttp);
        assert_eq!(spec.target(), "http://example.com:8443/mcp");
        Ok(())
    }

    /// The target a caller wrote is the target an adapter gets.
    ///
    /// `split_once(':')` finds the *scheme's* colon in `http://host/mcp`, so a
    /// parser that stops there hands the adapter `//host/mcp` — a
    /// protocol-relative URL, which `HttpClientConfig` rejects. This is the
    /// test that would have caught it, and it asserts on the whole string for
    /// the reason that matters: what leaves the parser is what the user wrote.
    #[test]
    fn a_url_target_survives_parsing_whole() -> Result<(), TestError> {
        for input in [
            "http://srv.example/mcp",
            "http-stateless://srv.example/mcp",
            "sse://old.example/sse",
        ] {
            let spec = TransportSpec::from_str(input)?;
            assert_eq!(spec.target(), input, "{input} did not survive parsing");
        }
        Ok(())
    }

    /// Displayed as one line that parses back to the same spec.
    ///
    /// The round trip is the claim, not the string: a user copies this into a
    /// shell, so a `Display` that differs from the accepted spelling is a
    /// display that sends them somewhere else.
    #[test]
    fn a_spec_displays_as_something_that_parses_back() -> Result<(), TestError> {
        for input in [
            "stdio:./server",
            "http://srv.example:8443/mcp",
            "sse:http://old.example/sse",
            "http-stateless:http://new.example/mcp",
        ] {
            let spec = TransportSpec::from_str(input)?;
            let shown = spec.to_string();
            assert_eq!(shown, input, "{input} displayed as {shown}");
            assert_eq!(
                TransportSpec::from_str(&shown)?,
                spec,
                "{shown} did not parse back to the same spec"
            );
        }
        Ok(())
    }

    /// Each malformed input has its own variant. Asserting on the variant is
    /// the point: a test that only checked "did it fail" would pass no matter
    /// how wrong the message was.
    #[test]
    fn malformed_input_is_refused_by_kind() -> Result<(), TestError> {
        let cases: [(&str, Expected); 4] = [
            ("", |e| matches!(e, SpecError::Empty)),
            ("   ", |e| matches!(e, SpecError::Empty)),
            ("carrier-pigeon", |e| {
                matches!(e, SpecError::UnknownTransport { .. })
            }),
            ("stdio:", |e| matches!(e, SpecError::EmptyTarget)),
        ];

        for (input, expected) in cases {
            let Err(refusal) = TransportSpec::from_str(input) else {
                return Err(format!("{input:?} should have been refused").into());
            };
            assert!(
                expected(&refusal),
                "{input:?} was refused as {refusal:?}, which is not its own case"
            );
        }
        Ok(())
    }

    /// `stdio` names a process, so `stdio` alone names nothing to connect to.
    /// `http` has an obvious default, so `http` alone is fine. The asymmetry is
    /// deliberate and is worth pinning.
    #[test]
    fn a_bare_stdio_spec_is_refused_and_a_bare_http_spec_is_not() -> Result<(), TestError> {
        let Err(refusal) = TransportSpec::from_str("stdio") else {
            return Err("a bare stdio spec should be refused".into());
        };
        assert_eq!(refusal, SpecError::MissingTarget { kind: "stdio" });

        let http = TransportSpec::from_str("http")?;
        assert_eq!(http.target(), "http://127.0.0.1:3000/mcp");
        Ok(())
    }

    /// `sse` is the legacy transport and also names a process or socket.
    #[test]
    fn a_bare_sse_spec_is_refused() {
        assert_eq!(
            TransportSpec::from_str("sse"),
            Err(SpecError::MissingTarget { kind: "sse" })
        );
    }

    /// Round-tripping through `Display` must reproduce what was parsed, or the
    /// error messages a bridge prints will name a transport that does not
    /// exist.
    #[test]
    fn display_reproduces_a_parsed_spec() -> Result<(), TestError> {
        for input in ["stdio:./server", "sse:http://old.example/sse"] {
            let spec = TransportSpec::from_str(input)?;
            assert_eq!(spec.to_string(), input);
        }
        Ok(())
    }
}
