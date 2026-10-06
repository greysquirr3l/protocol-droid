//! An [`AuthorizationSecrets`] that asks the operating system.
//!
//! [`AuthorizationSecrets`](crate::application::oauth::client::AuthorizationSecrets)
//! is a port with no production implementation in this crate, on purpose: the
//! right source depends on where the client runs, and a wrong default is a
//! predictable token rather than a compile error. This is the implementation to
//! copy instead.
//!
//! # Why 32 bytes, and why base64url
//!
//! RFC 7636 §7.1 asks for 32 octets of entropy in the `code_verifier`, and 32
//! is also what §4.1's length rule implies: base64url encodes three bytes into
//! four characters, so 32 bytes is 43 characters after the padding is dropped —
//! exactly the minimum, and not one character of it wasted on a `=`.
//!
//! The alphabet matters as much as the length does. RFC 7636 §4.1 admits only
//! the unreserved set, and the verifier is hashed with `ASCII` rather than
//! UTF-8, so a value outside ASCII is not a different verifier, it is a
//! different flow. Base64url's alphabet is `A-Z a-z 0-9 - _`, a subset of
//! unreserved and entirely ASCII, so the encoding cannot produce a value the
//! value object refuses. That is why [`SecretError::Rejected`] is documented as
//! an invariant breach rather than as a runtime possibility.
//!
//! The same encoding serves `state`, and there for a second reason: the value
//! travels back through a redirect URI query string, so it has to survive a URL
//! unchanged. Base64url is designed for that path and needs no percent-encoding;
//! a raw byte encoding would have to be escaped on the way out and unescaped on
//! the way back, and any asymmetry between the two is a flow that silently
//! fails its own `state` check.
//!
//! # Why the `state` is not derived from the verifier
//!
//! It could be — one draw, split in half — and it is tempting because it halves
//! the syscalls. It is not done here because `state` and `code_verifier` answer
//! different questions. `state` proves the response belongs to this request and
//! is compared by a client that may hold many outstanding flows; the verifier
//! is compared by the authorization server against the challenge it was sent.
//! Sharing entropy between them is safe, but sharing a *draw* couples their
//! lifetimes for no saving worth the coupling.
//!
//! # Errors
//!
//! [`getrandom`](getrandom::getrandom) is a runtime facility, not a link-time
//! constant, and it can fail — the operating system's entropy pool is not
//! always available, least of all early in boot. Both trait methods return the
//! failure rather than falling back to something weaker, because the fallback is
//! the bug this type exists to prevent: a guessable `state` accepts a callback
//! an attacker forged, and a guessable verifier is a code anyone can redeem.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use getrandom::getrandom;

use crate::application::oauth::client::{AuthorizationSecrets, SecretError, State};
use crate::domain::oauth::pkce::CodeVerifier;

/// RFC 7636 §7.1's suggested verifier entropy, and the draw size for `state`
/// as well. Both are drawn independently; see the module documentation.
const ENTROPY_BYTES: usize = 32;

/// Draws authorization secrets from the operating system's CSPRNG.
///
/// Zero-sized and stateless: every call reads fresh entropy, so there is
/// nothing to construct, seed, or keep alive. `Default` is safe here precisely
/// because there is no seed to get wrong — the warning in
/// [`RandomIds`](crate::adapters::secondary::id_source::RandomIds) about a
/// zero-seeded source does not apply to a type that never holds one.
///
/// # Examples
///
/// ```
/// use protocol_droid::adapters::secondary::oauth::secrets::OsSecrets;
/// use protocol_droid::application::oauth::client::AuthorizationSecrets;
///
/// let secrets = OsSecrets::default();
/// let state = secrets.new_state()?;
/// let verifier = secrets.new_code_verifier()?;
///
/// // 32 bytes of entropy, base64url-encoded, unpadded.
/// assert_eq!(verifier.as_str().len(), 43);
/// assert!(!state.as_str().is_empty());
/// # Ok::<(), protocol_droid::application::oauth::client::SecretError>(())
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct OsSecrets;

impl AuthorizationSecrets for OsSecrets {
    fn new_state(&self) -> Result<State, SecretError> {
        // `State` does not constrain its charset the way `CodeVerifier` does, so
        // nothing here can be rejected — but a state that is not URL-safe is a
        // state that fails to survive the redirect, so the encoding is the same
        // one used for the verifier rather than an arbitrary one.
        Ok(State::new(URL_SAFE_NO_PAD.encode(draw()?)))
    }

    fn new_code_verifier(&self) -> Result<CodeVerifier, SecretError> {
        // The value object re-checks length and charset. It cannot fail, and
        // `Rejected` says so: it is reported as an invariant breach rather than
        // swallowed, because a silent fallback here would be the exact
        // predictability bug this adapter is written to avoid.
        CodeVerifier::new(URL_SAFE_NO_PAD.encode(draw()?)).map_err(|_| SecretError::Rejected)
    }
}

/// [`ENTROPY_BYTES`] from the operating system's CSPRNG.
fn draw() -> Result<[u8; ENTROPY_BYTES], SecretError> {
    let mut bytes = [0_u8; ENTROPY_BYTES];
    // `getrandom::Error` is a struct over a reserved code space, not an enum, so
    // "this platform has no entropy source" is a specific constant rather than a
    // variant. It stays its own case because it is the one failure a
    // deployment cannot retry its way out of: there is nothing to retry, the
    // platform has no such facility.
    getrandom(&mut bytes).map_err(|error| match error {
        getrandom::Error::UNSUPPORTED => SecretError::EntropyUnsupported,
        _ => SecretError::EntropyUnavailable,
    })?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::{ENTROPY_BYTES, OsSecrets};
    use crate::application::oauth::client::{AuthorizationSecrets, SecretError};
    use crate::domain::oauth::pkce::ChallengeMethod;

    /// The two draws that must not repeat. Asserting a 256-bit draw differs is
    /// safe; the test fails only if the source has collapsed to a constant, and
    /// a constant is the failure this type exists to prevent.
    #[test]
    fn two_draws_are_not_the_same_value() -> Result<(), SecretError> {
        let secrets = OsSecrets::default();
        let first = secrets.new_code_verifier()?;
        let second = secrets.new_code_verifier()?;

        assert_ne!(first.as_str(), second.as_str());
        Ok(())
    }

    /// A counter — or a seeded counter, or one draw reused — passes a
    /// "not the same value" check on a single pair and fails here, because a
    /// counter advances and this does not.
    #[test]
    fn a_sequence_of_draws_does_not_advance() -> Result<(), SecretError> {
        let secrets = OsSecrets::default();
        let values: Vec<String> = (0..16)
            .map(|_| {
                secrets
                    .new_code_verifier()
                    .map(|verifier| verifier.as_str().to_owned())
            })
            .collect::<Result<_, SecretError>>()?;

        let mut sorted = values.clone();
        sorted.sort_unstable();

        // A counter would arrive already sorted; 16 random draws will not.
        assert_ne!(values, sorted, "the draws came out ordered: {values:?}");
        Ok(())
    }

    /// The verifier must survive the encoding this adapter chose, and come back
    /// to the entropy it started as. This is what makes the length claim in the
    /// module documentation checkable rather than asserted.
    #[test]
    fn a_verifier_decodes_back_to_the_entropy_it_was_drawn_from() -> Result<(), SecretError> {
        let secrets = OsSecrets::default();
        let verifier = secrets.new_code_verifier()?;

        let decoded = URL_SAFE_NO_PAD
            .decode(verifier.as_str())
            .map_err(|_| SecretError::Rejected)?;

        assert_eq!(decoded.len(), ENTROPY_BYTES);
        Ok(())
    }

    /// RFC 7636 §4.1 sets 43 as the minimum and 128 as the maximum. This
    /// adapter lands on the minimum exactly, which is what 32 bytes of entropy
    /// encodes to with the padding removed.
    #[test]
    fn a_verifier_is_exactly_the_rfc_minimum_length() -> Result<(), SecretError> {
        let secrets = OsSecrets::default();

        assert_eq!(secrets.new_code_verifier()?.as_str().len(), 43);
        Ok(())
    }

    /// The value object checks the charset on construction, so a verifier that
    /// built at all is already known to be within the unreserved set. Asserting
    /// it again here would only restate the constructor; asserting the encoded
    /// form is ASCII is the part the hash actually depends on, because RFC 7636
    /// §4.2 hashes `ASCII(code_verifier)` and a multi-byte character would
    /// silently produce a different digest than the server computes.
    #[test]
    fn a_verifier_is_ascii_and_url_safe() -> Result<(), SecretError> {
        let secrets = OsSecrets::default();
        let verifier = secrets.new_code_verifier()?;

        assert!(
            verifier.as_str().bytes().all(|byte| byte.is_ascii()),
            "the verifier is not ASCII: {}",
            verifier.as_str()
        );
        assert!(
            verifier
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
            "the verifier left the base64url alphabet: {}",
            verifier.as_str()
        );
        Ok(())
    }

    /// The challenge derived from a real verifier must itself be a legal S256
    /// value. This exercises the two halves together, which is the only way to
    /// show the encoding and the challenge derivation agree on the same bytes.
    #[test]
    fn a_drawn_verifier_yields_a_usable_challenge() -> Result<(), SecretError> {
        let secrets = OsSecrets::default();
        let verifier = secrets.new_code_verifier()?;

        let challenge = verifier.challenge(ChallengeMethod::S256);

        // S256 is BASE64URL(SHA256(ASCII(verifier))), so 32 bytes of digest is
        // 43 characters with no padding — the same shape as the verifier itself.
        assert_eq!(challenge.as_str().len(), 43);
        Ok(())
    }

    /// The state travels back through a redirect URI query string. A value that
    /// needed percent-encoding would be re-read in a different form on the way
    /// back and fail its own comparison.
    #[test]
    fn a_state_survives_a_url_query_unchanged() -> Result<(), SecretError> {
        let secrets = OsSecrets::default();
        let state = secrets.new_state()?;

        let encoded =
            url::form_urlencoded::byte_serialize(state.as_str().as_bytes()).collect::<String>();
        assert_eq!(encoded, state.as_str(), "the state was escaped: {encoded}");
        Ok(())
    }

    /// The two are drawn independently, so they are not equal even though the
    /// flow holds both at once.
    #[test]
    fn a_state_is_not_the_verifier() -> Result<(), SecretError> {
        let secrets = OsSecrets::default();

        assert_ne!(
            secrets.new_state()?.as_str(),
            secrets.new_code_verifier()?.as_str()
        );
        Ok(())
    }

    /// Every value the adapter produces must be usable as-is, which is the
    /// claim behind `SecretError::Rejected` being an invariant breach rather
    /// than a runtime possibility. Ninety draws, because a charset or length
    /// rule that failed one value in ninety would fail eventually in production.
    #[test]
    fn ninety_draws_are_all_accepted() -> Result<(), SecretError> {
        let secrets = OsSecrets::default();

        for _ in 0..90 {
            let _ = secrets.new_state()?;
            let _ = secrets.new_code_verifier()?;
        }
        Ok(())
    }

    /// Both trait methods report a failure rather than degrading. The
    /// implementation under test cannot be made to fail on demand, so this
    /// asserts the shape of the contract instead: the error type separates an
    /// environment that cannot supply entropy from an encoder that produced
    /// something illegal, and both are distinct from each other.
    #[test]
    fn an_entropy_failure_and_an_encoding_failure_are_distinguishable() {
        assert_ne!(
            SecretError::EntropyUnavailable,
            SecretError::EntropyUnsupported
        );
        assert_ne!(SecretError::EntropyUnavailable, SecretError::Rejected);
        assert_ne!(SecretError::EntropyUnsupported, SecretError::Rejected);
    }
}
