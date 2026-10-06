//! Proof Key for Code Exchange, as RFC 7636 specifies it.
//!
//! # What this is for
//!
//! The authorization code arrives in a URL — a redirect, a browser history, a
//! `Referer` header, an intermediary log. PKCE binds that code to the client
//! that asked for it: the client holds a `code_verifier` it never sends, and
//! the authorization server only issues the token to a redeemer that can
//! produce the matching `code_challenge`. A stolen code on its own is useless.
//!
//! # The grammar, from the RFC
//!
//! ```text
//! code-verifier = 43*128unreserved
//! unreserved = ALPHA / DIGIT / "-" / "." / "_" / "~"
//! ```
//!
//! [`CodeVerifier`] parses exactly that and refuses anything else, because a
//! verifier that does not satisfy the grammar is one some authorization servers
//! reject and others accept — the kind of difference that is a login failure
//! rather than an error message.
//!
//! ```text
//! plain  code_challenge = code_verifier
//! S256   code_challenge = BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))
//! ```
//!
//! `S256` is Mandatory To Implement on the server, so RFC 7636 §4.2 says a
//! capable client **MUST** use it and may use `plain` only where it cannot
//! support `S256` *and* has out-of-band knowledge that the server accepts it.
//! [`ChallengeMethod::Plain`] exists because the grammar has it, and
//! [`CodeVerifier::challenge`] does not stop a caller choosing it — the
//! judgement is a deployment's, not a library's.
//!
//! # The verifier is a secret
//!
//! It is exactly as sensitive as the token it will be exchanged for, and more
//! exposed: it crosses a public network on every authorization, while the token
//! may only cross a back channel. So [`CodeVerifier`] renders as `CodeVerifier(…)`
//! under [`Debug`](core::fmt::Debug) — the path a panic and an `assert_eq!`
//! expand to — and reserves [`Display`](core::fmt::Display) for putting one in
//! a token request body, which is the one place it legitimately has to appear.
//!
//! # Examples
//!
//! ```
//! use protocol_droid::domain::oauth::{ChallengeMethod, CodeVerifier};
//!
//! // The vector in RFC 7636 Appendix A.
//! let verifier = CodeVerifier::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")?;
//! let challenge = verifier.challenge(ChallengeMethod::S256);
//! assert_eq!(
//!     challenge.as_str(),
//!     "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
//! );
//! # Ok::<(), protocol_droid::domain::oauth::PkceError>(())
//! ```

use core::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// RFC 7636 §4.1: `code-verifier = 43*128unreserved`.
///
/// The bounds are counted in characters, which is unambiguous because §4.1's
/// `unreserved` is ASCII-only — there is no code point outside ASCII that the
/// grammar admits.
const VERIFIER_MIN: usize = 43;

/// RFC 7636 §4.1: the upper bound of `43*128unreserved`.
const VERIFIER_MAX: usize = 128;

/// A `code_verifier`: the value a client keeps and never sends until it redeems
/// the authorization code.
///
/// # Redaction
///
/// Renders as `CodeVerifier(…)` under [`Debug`](fmt::Debug). It is a secret
/// with the same lifetime and reach as the token it becomes, so it must not
/// escape through a panic message or an assertion failure. [`Display`](fmt::Display)
/// is the deliberate exception, because a client has to put this in a
/// `code_verifier` form field to redeem the code at all.
#[derive(Clone, PartialEq, Eq)]
pub struct CodeVerifier(String);

impl fmt::Debug for CodeVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CodeVerifier(…)")
    }
}

impl CodeVerifier {
    /// Parse a verifier against RFC 7636 §4.1's grammar.
    ///
    /// # Errors
    ///
    /// [`PkceError::VerifierLength`] if it is shorter than 43 or longer than
    /// 128 characters; [`PkceError::VerifierCharset`] if it contains anything
    /// outside `unreserved`.
    pub fn new(value: impl Into<String>) -> Result<Self, PkceError> {
        let value = value.into();
        let length = value.chars().count();
        if !(VERIFIER_MIN..=VERIFIER_MAX).contains(&length) {
            return Err(PkceError::VerifierLength { length });
        }
        // Every byte is checked rather than every character: the grammar is
        // defined over ASCII, so a multi-byte character is a charset failure
        // and should be reported as one rather than counted as several.
        let offending = value
            .as_bytes()
            .iter()
            .copied()
            .find(|byte| !is_unreserved(*byte));
        if let Some(byte) = offending {
            return Err(PkceError::VerifierCharset { byte });
        }
        Ok(Self(value))
    }

    /// The verifier, as it goes in a `code_verifier` form field.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The challenge this verifier produces under `method`.
    ///
    /// RFC 7636 §4.3. `Plain` returns the verifier unchanged; `S256` returns
    /// `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`, unpadded, which is
    /// 43 characters and always inside the `code_challenge` grammar.
    #[must_use]
    pub fn challenge(&self, method: ChallengeMethod) -> CodeChallenge {
        match method {
            ChallengeMethod::Plain => CodeChallenge(self.0.clone()),
            ChallengeMethod::S256 => {
                let digest = Sha256::digest(self.0.as_bytes());
                CodeChallenge(URL_SAFE_NO_PAD.encode(digest))
            }
        }
    }
}

/// Implements `fmt::Display` for a `CodeVerifier`.
///
/// This is the one path that prints the secret in full, and it exists so a
/// client can build a `code_verifier` field. Prefer
/// [`as_str`](CodeVerifier::as_str) at any call site that is not literally
/// serialising one into a token request.
impl fmt::Display for CodeVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// RFC 7636 §4.3's `code_challenge_method`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ChallengeMethod {
    /// `code_challenge = code_verifier`, sent as `plain`.
    ///
    /// Permitted by the grammar and by §4.3, but §4.2 says a client capable of
    /// `S256` **MUST** use it — `plain` is only for a client that cannot do
    /// better *and* knows the server accepts it. It provides no protection
    /// against an intercepted code, which is the whole point of the mechanism.
    Plain,
    /// `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`, sent as `S256`.
    ///
    /// Mandatory To Implement on the server per §4.2, so this is the default.
    #[default]
    S256,
}

impl ChallengeMethod {
    /// The wire spelling: `plain` or `S256`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::S256 => "S256",
        }
    }
}

impl fmt::Display for ChallengeMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A `code_challenge`: the public half of the exchange.
///
/// Not a secret — it travels in the authorization request, and an attacker
/// holding one cannot derive the verifier from it. [`Debug`] is therefore
/// derived, unlike [`CodeVerifier`]'s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeChallenge(String);

impl CodeChallenge {
    /// The challenge, as it goes in a `code_challenge` query parameter.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CodeChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A `code_verifier` that does not satisfy RFC 7636 §4.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PkceError {
    /// Outside `43*128unreserved`.
    #[error("a code verifier must be 43 to 128 characters, got {length}")]
    VerifierLength {
        /// The length actually supplied, counted in characters.
        length: usize,
    },

    /// Contains a byte `unreserved` does not admit.
    #[error(
        "a code verifier may only contain ALPHA, DIGIT, '-', '.', '_' and '~'; found 0x{byte:02X}"
    )]
    VerifierCharset {
        /// The first offending byte, as it appeared.
        byte: u8,
    },
}

/// RFC 7636 §4.1: `unreserved = ALPHA / DIGIT / "-" / "." / "_" / "~"`.
const fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

#[cfg(test)]
mod tests {
    use super::{ChallengeMethod, CodeVerifier, PkceError, VERIFIER_MAX, VERIFIER_MIN};

    /// The vector RFC 7636 Appendix A publishes, used verbatim so the
    /// derivation is checked against the document rather than against this
    /// crate's own idea of SHA-256 and base64url.
    const RFC_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const RFC_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    #[test]
    fn the_rfc_vector_derives_the_documented_challenge() -> Result<(), PkceError> {
        let verifier = CodeVerifier::new(RFC_VERIFIER)?;
        let challenge = verifier.challenge(ChallengeMethod::S256);
        assert_eq!(
            challenge.as_str(),
            RFC_CHALLENGE,
            "RFC 7636 Appendix A pins this derivation"
        );
        Ok(())
    }

    #[test]
    fn the_rfc_vector_is_at_the_grammar_minimum() {
        // 43 characters: if this ever changes, the vector is being truncated
        // rather than the grammar being read.
        assert_eq!(RFC_VERIFIER.chars().count(), VERIFIER_MIN);
        assert_eq!(RFC_CHALLENGE.chars().count(), VERIFIER_MIN);
    }

    #[test]
    fn plain_is_the_verifier_unchanged() -> Result<(), PkceError> {
        let verifier = CodeVerifier::new(RFC_VERIFIER)?;
        assert_eq!(
            verifier.challenge(ChallengeMethod::Plain).as_str(),
            RFC_VERIFIER
        );
        Ok(())
    }

    /// Both bounds of `43*128unreserved`, checked by construction rather than
    /// by asserting a length the parser happens to accept.
    #[test]
    fn both_length_bounds_are_enforced() {
        let shortest = "a".repeat(VERIFIER_MIN);
        let longest = "a".repeat(VERIFIER_MAX);
        assert!(
            CodeVerifier::new(shortest).is_ok(),
            "{VERIFIER_MIN} characters is the grammar minimum"
        );
        assert!(
            CodeVerifier::new(longest).is_ok(),
            "{VERIFIER_MAX} characters is the grammar maximum"
        );

        let too_short = "a".repeat(VERIFIER_MIN - 1);
        let too_long = "a".repeat(VERIFIER_MAX + 1);
        assert_eq!(
            CodeVerifier::new(too_short),
            Err(PkceError::VerifierLength {
                length: VERIFIER_MIN - 1
            })
        );
        assert_eq!(
            CodeVerifier::new(too_long),
            Err(PkceError::VerifierLength {
                length: VERIFIER_MAX + 1
            })
        );
    }

    /// Every character `unreserved` admits, and a representative sample of ones
    /// it does not. The boundary is easy to get subtly wrong, so it is walked
    /// rather than spot-checked.
    #[test]
    fn the_charset_is_exactly_unreserved() {
        /// A legal-length verifier whose first character is `first`.
        fn with_first(first: char) -> String {
            std::iter::once(first)
                .chain(std::iter::repeat_n('a', VERIFIER_MIN - 1))
                .collect()
        }

        for byte in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~" {
            assert!(
                CodeVerifier::new(with_first(char::from(*byte))).is_ok(),
                "0x{byte:02X} is unreserved and must be accepted"
            );
        }
        // Characters the grammar excludes. `+`, `/` and `=` are the ones that
        // matter in practice: they are what a padded base64 alphabet contains,
        // and a verifier that came from one fails at some servers and not
        // others. Space and `%` are a value that reached the wire unencoded.
        for byte in *b"+/= %*!" {
            assert_eq!(
                CodeVerifier::new(with_first(char::from(byte))),
                Err(PkceError::VerifierCharset { byte }),
                "0x{byte:02X} is not unreserved"
            );
        }
    }

    /// A non-ASCII character is a charset failure, not a length failure, even
    /// though it is more than one byte. Reported as the former because the
    /// remedy is different: a caller told it is too short will pad.
    #[test]
    fn a_multi_byte_character_is_a_charset_failure() {
        // "€" is three bytes, so a byte count would call this 45 characters.
        let candidate: String = std::iter::once('€')
            .chain(std::iter::repeat_n('a', VERIFIER_MIN - 1))
            .collect();
        assert_eq!(
            CodeVerifier::new(candidate),
            Err(PkceError::VerifierCharset { byte: 0xE2 })
        );
    }

    /// The verifier is a secret, and `Debug` is what a panic or an
    /// `assert_eq!` expands to.
    #[test]
    fn debug_never_renders_the_verifier() -> Result<(), PkceError> {
        let verifier = CodeVerifier::new(RFC_VERIFIER)?;
        let rendered = format!("{verifier:?}");
        assert_eq!(rendered, "CodeVerifier(…)");
        assert!(
            !rendered.contains(RFC_VERIFIER),
            "the verifier leaked: {rendered}"
        );
        Ok(())
    }

    /// `Display` is the deliberate exception: it is how a client builds the
    /// `code_verifier` field. Pinned here so making it redacting cannot happen
    /// without breaking the exchange.
    #[test]
    fn display_is_the_full_verifier() -> Result<(), PkceError> {
        let verifier = CodeVerifier::new(RFC_VERIFIER)?;
        assert_eq!(verifier.to_string(), RFC_VERIFIER);
        assert_eq!(verifier.as_str(), RFC_VERIFIER);
        Ok(())
    }

    #[test]
    fn the_method_spells_itself_the_way_the_rfc_writes_it() {
        assert_eq!(ChallengeMethod::Plain.as_str(), "plain");
        assert_eq!(ChallengeMethod::S256.as_str(), "S256");
        // S256 is the default because §4.2 makes it mandatory for a capable
        // client; defaulting the other way would make the unsafe choice the
        // one a caller gets for writing nothing.
        assert_eq!(ChallengeMethod::default(), ChallengeMethod::S256);
    }

    /// An S256 challenge is 43 characters regardless of the verifier, because
    /// SHA-256 is 32 bytes and unpadded base64url of 32 bytes is 43. Both
    /// bounds of the challenge grammar therefore hold for every input.
    #[test]
    fn an_s256_challenge_is_always_the_right_size() -> Result<(), PkceError> {
        for length in [VERIFIER_MIN, VERIFIER_MIN + 1, VERIFIER_MAX] {
            let verifier = CodeVerifier::new("a".repeat(length))?;
            let challenge = verifier.challenge(ChallengeMethod::S256);
            assert_eq!(
                challenge.as_str().chars().count(),
                VERIFIER_MIN,
                "a challenge derived from a {length}-character verifier"
            );
        }
        Ok(())
    }
}
