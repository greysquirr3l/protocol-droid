//! The URI a `2024-11-05` server hands the client in its `endpoint` event.
//!
//! # What the specification says, and what it does not
//!
//! One sentence, from the `2024-11-05` transports page:
//!
//! > When a client connects, the server MUST send an `endpoint` event
//! > containing a URI for the client to use for sending messages.
//!
//! Cited: <https://modelcontextprotocol.io/specification/2024-11-05/basic/transports>
//!
//! That is the whole of it. What is **absent** matters more than what is
//! present — there is no `sessionId` query parameter and no `202 Accepted`
//! in the specification. Those are SDK conventions, and a
//! crate that cites the spec has to say so rather than quietly encode a
//! convention and attribute it to the standard.
//!
//! # Why the client must not parse the query
//!
//! Because the server chose the URI, and the client is not entitled to know
//! how. A client that pulled `sessionId` out of the query and rebuilt the URL
//! works against the reference server and breaks against every server that
//! encodes the session differently — a path segment, a cookie, a header.
//! [`MessageEndpoint`] exists to be used **verbatim**: resolve it against the
//! URL the client already dialled and POST there. The only thing the client
//! looks at is whether the thing it was given is safe to follow, which is the
//! next section.
//!
//! # Why this refuses an absolute URI
//!
//! Because the value arrives from the peer. A server — or anything that has
//! become one by answering on its port — hands the client a string, and the
//! natural client behaviour is "POST my JSON-RPC where the server told me to".
//! If that string may be an absolute URL, the endpoint event is a
//! server-side request forgery primitive: point it at
//! `http://169.254.169.254/latest/meta-data/` and the client will send the
//! request, with whatever headers and session it holds, to somewhere the user
//! never configured and cannot see. The specification does not mention this
//! because the specification assumes the server is the thing you are talking
//! to; it does not consider what happens when a dev server is reached over
//! plain HTTP by something else first.
//!
//! A *relative* URI cannot do this. `/messages?sessionId=abc` resolves against
//! the origin the client already dialled, and there is nothing in it to
//! redirect. So the parse refuses the two shapes that escape the origin —
//! [`Absolute`](MessageEndpointError::Absolute) and
//! [`ProtocolRelative`](MessageEndpointError::ProtocolRelative) — and the
//! server is left to encode whatever it likes in a form that stays inside the
//! origin it is already serving.
//!
//! The other two refusals are ordinary framing hygiene, and the parser is
//! where they belong: a C0 control or a space in a request target is a
//! request-line injection, and a target that does not start with `/` is not a
//! path this server would have served.

use core::fmt;
use core::str::FromStr;

use thiserror::Error;

/// The `endpoint` event's data, parsed once and used verbatim.
///
/// A newtype over the whole target rather than a decomposed
/// `(path, query, session)`. Decomposing is the mistake: it makes the client's
/// next move "rebuild the URL", and rebuilding is where a server's choice of
/// encoding becomes this client's bug.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageEndpoint(String);

impl MessageEndpoint {
    /// The target, exactly as the server wrote it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The target's path, without any query.
    ///
    /// For a server logging what it offered, and for a test asserting the
    /// shape. A *client* has no use for this: it POSTs to
    /// [`as_str`](Self::as_str).
    #[must_use]
    pub fn path(&self) -> &str {
        match self.0.split_once('?') {
            Some((path, _query)) => path,
            None => &self.0,
        }
    }

    /// The query string, without the `?`.
    #[must_use]
    pub fn query(&self) -> Option<&str> {
        self.0.split_once('?').map(|(_path, query)| query)
    }

    /// A path with no query and no fragment, for a server that will append one.
    ///
    /// Distinct from [`FromStr`] because a *configured* path and an *advertised*
    /// endpoint are different shapes, and only one of them may carry a query.
    /// A path of `/messages?tenant=a` parses fine as a target — a client could
    /// be told to post there — but a server that then appends `sessionId` to it
    /// produces `/messages?tenant=a?sessionId=1`, whose second `?` is data
    /// rather than a delimiter, and a client that splits on it gets a query of
    /// `a?sessionId=1` and posts with no session at all. A `#` is worse: it
    /// makes the whole query a fragment, which the client drops silently.
    ///
    /// Refusing both here is what lets [`with_query_parameter`](Self::with_query_parameter)
    /// be infallible.
    ///
    /// # Errors
    ///
    /// [`MessageEndpointError::NotAbsolutePath`] for an empty value, a
    /// non-`/`-initial one, one that is a query string, or one that names a
    /// scheme; [`MessageEndpointError::IllegalCharacter`] for a C0 control,
    /// DEL, or a space; and [`MessageEndpointError::QueryOrFragment`] for a
    /// value that already carries a `?` or a `#`.
    pub fn bare_path(value: &str) -> Result<Self, MessageEndpointError> {
        // The general parse first, the `?`/`#` rule second, so that each value
        // is refused for the most specific thing wrong with it. A value of
        // `?sessionId=1` has no path at all, and `Empty` says that where
        // `QueryOrFragment` would say something true and less useful.
        let endpoint = value.parse::<Self>()?;
        if value.contains(['?', '#']) {
            return Err(MessageEndpointError::QueryOrFragment);
        }
        Ok(endpoint)
    }

    /// This target with one more query parameter appended.
    ///
    /// Infallible, and that is the whole point: the result is a string this
    /// module assembled from a [`bare_path`](Self::bare_path) it already
    /// validated and a key and value it is choosing, so there is nothing left
    /// to parse and no way for the result to be one [`FromStr`] would refuse.
    ///
    /// An existing query is preserved rather than replaced — `?tenant=a` plus
    /// `sessionId=1` is `?tenant=a&sessionId=1` — because the alternative is a
    /// server that silently discards its own configuration the first time a
    /// client is added.
    ///
    /// `key` and `value` are not validated here, and cannot be: the only
    /// callers in this crate pass a `&'static str` constant and a hexadecimal
    /// session id. A caller with an arbitrary string should parse it into a
    /// [`MessageEndpoint`] instead of reaching for this.
    #[must_use]
    pub fn with_query_parameter(&self, key: &str, value: &str) -> Self {
        let separator = if self.query().is_some() { '&' } else { '?' };
        Self(format!("{}{separator}{key}={value}", self.0))
    }
}

/// Why a string is not a usable message endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MessageEndpointError {
    /// The value was empty, or was only a query string.
    #[error("a message endpoint may not be empty")]
    Empty,

    /// The value named a scheme, so it could point anywhere.
    ///
    /// See the module docs: this is the refusal that makes the `endpoint`
    /// event unusable as an SSRF primitive. Reported separately from
    /// [`ProtocolRelative`](Self::ProtocolRelative) because the two have
    /// different shapes in a log and a reader wants to know which one a peer
    /// tried.
    #[error(
        "a message endpoint may not be an absolute URI; it must stay on the origin the client dialled"
    )]
    Absolute,

    /// The value began `//`, which is a protocol-relative URL.
    ///
    /// Its own case and not an [`Absolute`](Self::Absolute) one because it has
    /// no scheme to give it away: `//evil.example/steal` resolves to
    /// `https://evil.example/steal` under the client's own scheme, so a parser
    /// that only looked for `://` would walk straight past it.
    #[error(
        "a message endpoint may not be protocol-relative; it must stay on the origin the client dialled"
    )]
    ProtocolRelative,

    /// The value contained a control character or a space.
    ///
    /// A request target with a CR in it is a request-line injection: a
    /// conforming client cannot send it, and an lenient one can be made to
    /// send a second request the user never asked for.
    #[error("a message endpoint may not contain a control character or a space")]
    IllegalCharacter,

    /// The value already carried a `?` or a `#`, so a query appended to it
    /// would be part of the existing query or of a fragment.
    #[error("a configured message path must not carry a query or a fragment")]
    QueryOrFragment,

    /// The value did not begin with `/`.
    #[error("a message endpoint must be an absolute path starting with `/`")]
    NotAbsolutePath,
}

impl FromStr for MessageEndpoint {
    type Err = MessageEndpointError;

    /// Parses a target the client can safely resolve against the origin it
    /// already dialled.
    ///
    /// # Errors
    ///
    /// - [`MessageEndpointError::Empty`] for an empty target or a bare `?`.
    /// - [`MessageEndpointError::ProtocolRelative`] for a `//` prefix, which
    ///   carries no scheme and so escapes a check that only looks for one.
    /// - [`MessageEndpointError::Absolute`] for anything with a scheme. The
    ///   check is on the scheme *syntax*, not on the literal `://`, so
    ///   `mailto:x` and `data:text/plain,x` are caught as well as
    ///   `https://host/`.
    /// - [`MessageEndpointError::IllegalCharacter`] for a C0 control, DEL, or
    ///   a space.
    /// - [`MessageEndpointError::NotAbsolutePath`] for a target that is
    ///   neither empty nor `/`-initial: a relative `messages?sessionId=x` is
    ///   ambiguous against whatever the client's current path happens to be,
    ///   and resolving it the way a browser would is not a behaviour a
    ///   protocol should depend on.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || value == "?" || value.starts_with('?') {
            return Err(MessageEndpointError::Empty);
        }
        // Before the scheme check, because `//host/x` has no scheme and is the
        // shape a scheme-only check misses.
        if value.starts_with("//") {
            return Err(MessageEndpointError::ProtocolRelative);
        }
        if has_scheme(value) {
            return Err(MessageEndpointError::Absolute);
        }
        if value
            .chars()
            .any(|c| c.is_control() || c == ' ' || c == '\u{7f}')
        {
            return Err(MessageEndpointError::IllegalCharacter);
        }
        if !value.starts_with('/') {
            return Err(MessageEndpointError::NotAbsolutePath);
        }
        Ok(Self(value.to_string()))
    }
}

/// Whether `value` begins with a URI scheme.
///
/// `scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"`, per RFC 3986
/// §3.1 — hand-written rather than pulled in, because the alternative is
/// taking a URI parser to answer a question that is one regex-shaped
/// predicate, and because the interesting failure is the *false negative*: a
/// check that looked for `://` would pass `data:text/plain,hello` and this
/// parser has to refuse it.
fn has_scheme(value: &str) -> bool {
    let Some(colon) = value.find(':') else {
        return false;
    };
    let scheme = value.split_at(colon).0;
    let mut chars = scheme.chars();
    // ALPHA first: a scheme cannot begin with a digit or a sign, and
    // `/messages?sessionId=a:b` has a colon whose prefix is not a scheme.
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

impl fmt::Display for MessageEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    /// A configured path is not a target: it has to be able to *take* a query.
    ///
    /// A table rather than a loop over cases with an index, because the cases
    /// are the documentation here — a reader deciding whether the `?` and `#`
    /// refusals are over-strict should be able to read the shapes rather than
    /// the loop that rejects them.
    ///
    /// Each case names the *variant*, not merely that something went wrong. A
    /// test that checks only "it failed" passes when a validator refuses for
    /// the wrong reason, which is how a good message slowly becomes a
    /// misleading one.
    #[test]
    fn a_configured_path_may_not_already_carry_a_query_or_a_fragment() {
        use MessageEndpointError as E;
        let cases = [
            ("/messages", None),
            ("/messages?sessionId=1", Some(E::QueryOrFragment)),
            ("/messages#a", Some(E::QueryOrFragment)),
            ("/messages#a?b=c", Some(E::QueryOrFragment)),
            ("?sessionId=1", Some(E::Empty)),
            ("https://elsewhere.example/x", Some(E::Absolute)),
            ("//elsewhere.example/x", Some(E::ProtocolRelative)),
            ("messages", Some(E::NotAbsolutePath)),
            ("/mess ages", Some(E::IllegalCharacter)),
        ];
        for (value, wanted) in cases {
            assert_eq!(
                MessageEndpoint::bare_path(value).err(),
                wanted,
                "bare_path({value:?})"
            );
        }
    }

    /// Appending must not cost the server its own configuration.
    ///
    /// A path of `/messages?tenant=a` is refused as a *configured* path — see
    /// the test above — but a `MessageEndpoint` built some other way can carry
    /// a query, and then this is the method that has to preserve it. Replacing
    /// rather than extending would silently drop `tenant`, and the symptom
    /// would be a client talking to the wrong tenant with no error anywhere.
    #[test]
    fn appending_extends_an_existing_query_rather_than_replacing_it() -> Result<(), TestError> {
        let endpoint = MessageEndpoint::from_str("/messages?tenant=a")?;
        let with_session = endpoint.with_query_parameter("sessionId", "0000000000000001");
        assert_eq!(
            with_session.as_str(),
            "/messages?tenant=a&sessionId=0000000000000001"
        );
        assert_eq!(with_session.path(), "/messages");
        assert_eq!(
            with_session.query(),
            Some("tenant=a&sessionId=0000000000000001")
        );
        Ok(())
    }

    /// And with no query, the first parameter is what introduces one.
    #[test]
    fn appending_to_a_bare_path_introduces_the_query() -> Result<(), TestError> {
        let endpoint = MessageEndpoint::from_str("/messages")?;
        let with_session = endpoint.with_query_parameter("sessionId", "0000000000000001");
        assert_eq!(
            with_session.as_str(),
            "/messages?sessionId=0000000000000001"
        );
        assert_eq!(with_session.path(), "/messages");
        assert_eq!(with_session.query(), Some("sessionId=0000000000000001"));
        Ok(())
    }

    use super::{MessageEndpoint, MessageEndpointError};
    use core::str::FromStr;

    type TestError = Box<dyn std::error::Error>;

    /// Asserts `value` parses, and reads back as `expected`.
    fn accepts(value: &str, expected: &str) -> Result<(), TestError> {
        let endpoint = MessageEndpoint::from_str(value)?;
        if endpoint.as_str() != expected {
            return Err(format!("{value:?} read back as {:?}", endpoint.as_str()).into());
        }
        Ok(())
    }

    /// Asserts `value` is refused, and with `expected`.
    fn refuses(value: &str, expected: &MessageEndpointError) -> Result<(), TestError> {
        match MessageEndpoint::from_str(value) {
            Err(error) if &error == expected => Ok(()),
            Err(error) => {
                Err(format!("{value:?} refused as {error:?}, wanted {expected:?}").into())
            }
            Ok(endpoint) => Err(format!(
                "{value:?} was accepted as {:?}, which can leave the origin",
                endpoint.as_str()
            )
            .into()),
        }
    }

    /// The shape the reference server sends.
    #[test]
    fn the_reference_shape_parses() -> Result<(), TestError> {
        accepts(
            "/messages?sessionId=0a1b2c3d",
            "/messages?sessionId=0a1b2c3d",
        )
    }

    #[test]
    fn a_plain_path_parses() -> Result<(), TestError> {
        accepts("/messages", "/messages")
    }

    /// The reason the client uses the target verbatim.
    #[test]
    fn a_path_and_query_are_kept_together() -> Result<(), TestError> {
        let endpoint = MessageEndpoint::from_str("/messages?sessionId=x&y=z")?;
        if endpoint.path() != "/messages" {
            return Err(format!("path read as {:?}", endpoint.path()).into());
        }
        if endpoint.query() != Some("sessionId=x&y=z") {
            return Err(format!("query read as {:?}", endpoint.query()).into());
        }
        // And the whole thing is what a client would POST to, unchanged.
        if endpoint.as_str() != "/messages?sessionId=x&y=z" {
            return Err("the target was rebuilt rather than kept".into());
        }
        Ok(())
    }

    /// The `SSRF` case. A client that sent its `JSON-RPC` here would be sending
    /// the session to a host the user never configured.
    #[test]
    fn an_absolute_url_is_refused() -> Result<(), TestError> {
        refuses(
            "http://169.254.169.254/latest/meta-data/",
            &MessageEndpointError::Absolute,
        )
    }

    /// `data:` has no `//` and is still an absolute URI, so a check that only
    /// looked for `://` would let it through.
    #[test]
    fn a_schemed_url_without_slashes_is_refused() -> Result<(), TestError> {
        refuses("data:text/plain,hello", &MessageEndpointError::Absolute)
    }

    /// The shape that carries no scheme at all, and so escapes a scheme check.
    #[test]
    fn a_protocol_relative_url_is_refused() -> Result<(), TestError> {
        refuses(
            "//evil.example/steal",
            &MessageEndpointError::ProtocolRelative,
        )
    }

    /// A CR in a request target is a request-line injection.
    #[test]
    fn a_control_character_is_refused() -> Result<(), TestError> {
        refuses(
            "/messages\r\nX-Injected: 1",
            &MessageEndpointError::IllegalCharacter,
        )
    }

    #[test]
    fn a_space_is_refused() -> Result<(), TestError> {
        refuses(
            "/messages and more",
            &MessageEndpointError::IllegalCharacter,
        )
    }

    /// A colon whose prefix is not a scheme is not a scheme. A server may
    /// perfectly well use a session id that looks like one, and refusing it
    /// would be refusing a legal frame.
    #[test]
    fn a_colon_that_is_not_a_scheme_is_kept() -> Result<(), TestError> {
        accepts(
            "/messages?sessionId=2024-11-05T00:00:00Z",
            "/messages?sessionId=2024-11-05T00:00:00Z",
        )
    }

    /// RFC 3986 §3.1: a scheme starts with `ALPHA`. `9:foo` has the *shape* of
    /// a scheme and is not one, so it is a relative path — and has to be
    /// reported as one.
    ///
    /// This test exists because the ALPHA check was **unpinned**: deleting it
    /// left all sixteen tests green, since a digit-led colon is refused either
    /// way and only the reason changes. The reason is what a client reading a
    /// refusal log actually looks at, and a parser that blames a peer for
    /// pointing off-origin when the peer sent a relative path produces a log
    /// nobody trusts.
    #[test]
    fn a_scheme_may_not_start_with_a_digit() -> Result<(), TestError> {
        refuses("9:foo", &MessageEndpointError::NotAbsolutePath)
    }

    /// The same for the characters RFC 3986 excludes from a leading position.
    #[test]
    fn a_scheme_may_not_start_with_a_sign_or_a_dot() -> Result<(), TestError> {
        for target in ["-x:foo", "+x:foo", ".x:foo"] {
            refuses(target, &MessageEndpointError::NotAbsolutePath)?;
        }
        Ok(())
    }

    /// Relative targets are ambiguous against whatever path the client has
    /// dialled, and a protocol should not depend on browser resolution rules.
    #[test]
    fn a_relative_path_is_refused() -> Result<(), TestError> {
        refuses(
            "messages?sessionId=x",
            &MessageEndpointError::NotAbsolutePath,
        )
    }

    #[test]
    fn an_empty_target_is_refused() -> Result<(), TestError> {
        refuses("", &MessageEndpointError::Empty)
    }

    /// A bare query names no path at all.
    #[test]
    fn a_bare_query_is_refused() -> Result<(), TestError> {
        refuses("?sessionId=x", &MessageEndpointError::Empty)
    }
}
