//! RFC 9207 issuer identification, checked on the authorization response.
//!
//! # What a mix-up attack is
//!
//! A client that talks to more than one authorization server sends
//! authorization requests it cannot tell apart once the responses come back. An
//! attacker who can interpose routes a request intended for server A to server
//! B, and returns B's response to a client that believes it is finishing with
//! A. The client has an authorization code and a token endpoint it trusts, and
//! no way to notice the two do not belong together — RFC 9207 §1 calls this a
//! mix-up attack and the `iss` parameter the countermeasure.
//!
//! The client already knows which issuer it asked, from the discovery
//! document it started at. So the check is a comparison against something the
//! client already holds, not a lookup.
//!
//! # Two rules that are easy to get wrong
//!
//! **Simple string comparison.** RFC 9207 §2.1 says the comparison "MUST use
//! simple string comparison". Not URI normalisation, not case folding, not
//! trailing-slash trimming. A server that sends an issuer differing only in
//! normalisation from the one discovered is either doing something this
//! specification does not describe or is an impostor, and quietly normalising
//! would accept the second.
//!
//! **Absent depends on what the server advertised.** The authorization page's
//! table (SEP-2468, RFC 9207 §2.4) decides it: a server that sets
//! `authorization_response_iss_parameter_supported: true` and then omits `iss`
//! is refused, and a server that does not advertise it may omit `iss`. A
//! present `iss` is compared either way.
//! <https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization#authorization-response-validation>
//! [`check_response_issuer`] is the not-advertised row, kept for callers that
//! have no metadata to read the flag from; a client that discovered the server
//! calls [`check_response_issuer_advertised`].

use crate::domain::mcp::authorization::AuthorizationModel;

/// Why an authorization response was refused.
///
/// Owned strings rather than borrows: the caller has `&str` for both issuers and
/// has no reason to be told it must keep them alive to read the error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorizationResponseError {
    /// The response named an issuer other than the one the client asked.
    ///
    /// RFC 9207 §2.1: the client "MUST reject the authorization response and
    /// MUST NOT proceed with the authorization grant". The classic mix-up —
    /// the code and the token endpoint now belong to different servers.
    #[error(
        "the authorization response names issuer `{presented}`, but this client asked `{expected}`"
    )]
    IssuerMismatch {
        /// The issuer the client recorded when it started the flow.
        expected: String,
        /// The issuer the response claimed.
        presented: String,
    },

    /// The server advertised `authorization_response_iss_parameter_supported`
    /// and the response carried no `iss`.
    ///
    /// RFC 9207 §2.4: such a server must always send it, so a response
    /// without one did not come from the server the client asked.
    #[error("the authorization server advertised `iss` and the response omitted it")]
    IssuerMissing,
}

/// Check an authorization response's `iss` against the issuer the client asked.
///
/// The not-advertised row: returns `Ok(())` for a response that carries no
/// `iss`, and for one that
/// carries the expected issuer. Anything else is a mix-up attempt and the
/// client must not proceed with the grant.
///
/// The comparison is RFC 9207 §2.1's simple string comparison — see the module
/// documentation for why it is not a URI equality.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::ProtocolVersion;
/// use protocol_droid::domain::mcp::authorization::authorization_model;
/// use protocol_droid::domain::oauth::check_response_issuer;
///
/// let model = authorization_model(ProtocolVersion::V2026_07_28);
/// let expected = "https://auth.example.com";
///
/// // Absent is tolerated; SEP-2468 makes the obligation validate-a-present-one.
/// assert!(check_response_issuer(&model, expected, None).is_ok());
/// assert!(check_response_issuer(&model, expected, Some(expected)).is_ok());
/// assert!(check_response_issuer(&model, expected, Some("https://elsewhere")).is_err());
/// ```
pub fn check_response_issuer(
    model: &AuthorizationModel,
    expected: &str,
    presented: Option<&str>,
) -> Result<(), AuthorizationResponseError> {
    if !model.issuer_parameter {
        return Ok(());
    }
    match presented {
        None => Ok(()),
        // Simple string comparison, per §2.1. Not `eq_ignore_ascii_case`, not
        // a normalised URI — see the module documentation.
        Some(presented) if presented == expected => Ok(()),
        Some(presented) => Err(AuthorizationResponseError::IssuerMismatch {
            expected: expected.to_owned(),
            presented: presented.to_owned(),
        }),
    }
}

/// [`check_response_issuer`], knowing whether the server advertised
/// `authorization_response_iss_parameter_supported`.
///
/// # Errors
///
/// [`AuthorizationResponseError::IssuerMissing`] for an advertising server
/// that omitted `iss`, and [`AuthorizationResponseError::IssuerMismatch`] for
/// a present `iss` that is not the expected one.
pub fn check_response_issuer_advertised(
    model: &AuthorizationModel,
    expected: &str,
    presented: Option<&str>,
    advertised: bool,
) -> Result<(), AuthorizationResponseError> {
    if model.issuer_parameter && advertised && presented.is_none() {
        return Err(AuthorizationResponseError::IssuerMissing);
    }
    check_response_issuer(model, expected, presented)
}

#[cfg(test)]
mod tests {
    use super::{AuthorizationResponseError, check_response_issuer};
    use crate::domain::ProtocolVersion;
    use crate::domain::mcp::authorization::{AuthorizationModel, authorization_model};

    const AS: &str = "https://auth.example.com";

    fn model_for(version: ProtocolVersion) -> AuthorizationModel {
        authorization_model(version)
    }

    #[test]
    fn a_matching_issuer_is_accepted() {
        let model = model_for(ProtocolVersion::V2026_07_28);
        assert!(check_response_issuer(&model, AS, Some(AS)).is_ok());
    }

    /// SEP-2468: the client's obligation is to validate a *present* `iss`.
    /// Absent is tolerated even on the revision that introduces the parameter.
    #[test]
    fn an_absent_issuer_is_tolerated() {
        for version in [ProtocolVersion::V2025_11_25, ProtocolVersion::V2026_07_28] {
            let model = model_for(version);
            assert!(
                check_response_issuer(&model, AS, None).is_ok(),
                "{version:?} tolerates an absent iss"
            );
        }
    }

    /// The mix-up. A response from a different server is fatal even though the
    /// code and the flow are otherwise well formed.
    #[test]
    fn a_different_issuer_is_rejected() {
        let model = model_for(ProtocolVersion::V2026_07_28);
        let outcome = check_response_issuer(&model, AS, Some("https://auth.attacker.example"));
        assert_eq!(
            outcome,
            Err(AuthorizationResponseError::IssuerMismatch {
                expected: String::from(AS),
                presented: String::from("https://auth.attacker.example"),
            })
        );
    }

    /// RFC 9207 §2.1 requires *simple string comparison*. Each of these differs
    /// from `AS` only under a normalisation rule, and each must be rejected —
    /// accepting one is what lets an impostor through.
    #[test]
    fn normalisation_does_not_make_a_different_issuer_match() {
        let model = model_for(ProtocolVersion::V2026_07_28);
        for presented in [
            "https://auth.example.com/",
            "https://AUTH.EXAMPLE.COM",
            "https://auth.example.com:443",
            "http://auth.example.com",
            " https://auth.example.com",
            "https://auth.example.com#",
        ] {
            assert!(
                check_response_issuer(&model, AS, Some(presented)).is_err(),
                "`{presented}` differs only by normalisation and must be rejected"
            );
        }
    }

    /// Before `2026-07-28` the parameter is nobody's obligation, so a response
    /// is not examined for it at all — including when it is present and wrong.
    #[test]
    fn a_revision_that_does_not_require_the_parameter_does_not_check_it() {
        for version in [
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
        ] {
            let model = model_for(version);
            assert!(
                !model.issuer_parameter,
                "{version:?} must not require issuer validation"
            );
            assert!(
                check_response_issuer(&model, AS, Some("https://auth.attacker.example")).is_ok(),
                "{version:?} does not inspect iss"
            );
        }
    }
}
