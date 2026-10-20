//! What a JWT access token is allowed to say, and which key may vouch for it.
//!
//! Two kinds of test live here, and the split is deliberate. The **selection**
//! rules are tested against key sets built inline, because every one of them is
//! reachable without a signature: a key marked `enc` is refused before any
//! cryptography runs, so a test that had to sign something would be testing
//! less and taking longer. The **end-to-end** rules — a signed token really
//! does verify, a tampered one really does not — are tested against a token
//! signed here, which is the only way to know the accept path works at all.
//!
//! The signing key is `HS256`, not RSA, and that is a choice rather than a
//! shortcut. An RSA fixture needs a private key to sign with, and a hardcoded
//! one in a test file is a private key in a repository. HMAC needs the same
//! bytes to sign and to verify, which makes the round trip honest. The
//! RSA-specific behaviour that actually matters — that an RSA key cannot verify
//! an HMAC token — is covered by the confusion test, which reaches the rejection
//! without needing to sign anything.

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use base64::Engine as _;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::json;

use crate::application::oauth::{Refusal, ScopePolicy, SourceError, TokenClaims, TokenSource};
use crate::domain::registry::{Scope, ScopeSet};

use super::claims::{JwtClaims, instant};
use super::key_set::{KeyError, select};
use super::{JwtSource, default_algorithms};

/// The shared secret the fixtures sign with and the published set advertises.
const SECRET: &[u8] = b"a-test-key-that-is-not-a-real-secret";

/// The `kid` the published set uses.
const KID: &str = "current";

/// Every error these tests answer, collected so `?` works and a failure prints
/// the engine's own account of what it refused.
#[derive(Debug, thiserror::Error)]
enum Failure {
    /// A fixture was not valid JSON, or a claim set was not a claim set.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// A fixture could not be signed.
    #[error(transparent)]
    Encode(#[from] jsonwebtoken::errors::Error),
    /// A token was refused, or could not be established.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// `verify` refused a token that reached it intact.
    #[error(transparent)]
    Refused(#[from] Refusal),
    /// A resource identifier did not parse.
    #[error(transparent)]
    Uri(#[from] crate::domain::mcp::ResourceUriError),
    /// A key could not be selected.
    #[error(transparent)]
    Key(#[from] KeyError),
}

/// A published set built from JWK literals, so a rule like `use: enc` is
/// expressed as the wire bytes a real authorization server would publish rather
/// than as a field assignment on a parsed object.
fn set(keys: Vec<serde_json::Value>) -> Result<JwkSet, Failure> {
    Ok(JwkSet {
        keys: keys
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

/// A published set holding one HMAC key named [`KID`].
fn published() -> Result<JwkSet, Failure> {
    set(vec![json!({
        "kty": "oct",
        "kid": KID,
        "alg": "HS256",
        "k": secret_b64(),
    })])
}

/// [`SECRET`] in the RFC 7517 §4.5 `k` encoding.
///
/// Derived rather than written out, so the published key and the signing key
/// cannot drift apart and fail for a reason that reads like a crypto bug.
fn secret_b64() -> String {
    base64url(SECRET)
}

fn claims_json() -> serde_json::Value {
    json!({
        "sub": "alice",
        "iss": "https://auth.example",
        "aud": "https://mcp.example/mcp",
        "exp": 1_700_000_060,
        "scope": "mcp:tools",
    })
}

/// A token signed with [`SECRET`], carrying `claims`.
fn signed(claims: &serde_json::Value) -> Result<String, Failure> {
    Ok(encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(SECRET),
    )?)
}

/// A token signed with [`SECRET`] and naming `kid`.
fn signed_named(claims: &serde_json::Value, kid: &str) -> Result<String, Failure> {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(kid.to_owned());
    Ok(encode(&header, claims, &EncodingKey::from_secret(SECRET))?)
}

/// A header naming [`KID`] and using `alg`.
fn header(alg: Algorithm) -> Header {
    let mut header = Header::new(alg);
    header.kid = Some(KID.to_owned());
    header
}

/// A source whose key set is seeded rather than fetched.
fn source_over(keys: JwkSet, accepted: Vec<Algorithm>) -> JwtSource {
    let source = JwtSource::new("http://127.0.0.1:1/unused.json", accepted);
    source.seed(keys);
    source
}

/// The scope strings a mapped claim carries, as `&str`.
///
/// `TokenClaims::scopes` hands back `&[Arc<str>]`, which does not compare
/// against `&str`; converting here keeps the tests reading as the claim they are
/// about rather than as the type that carries it.
fn scope_strings(claims: &TokenClaims) -> Vec<&str> {
    claims.scopes().iter().map(Arc::as_ref).collect()
}

/// Maps every `mcp:`-prefixed scope to `Read`.
///
/// Only the one test that goes as far as an `AuthContext` needs it; everything
/// else stops at the claims.
struct Permissive;

impl ScopePolicy for Permissive {
    fn resolve(&self, scope: &str) -> Option<Scope> {
        scope.starts_with("mcp:").then_some(Scope::Read)
    }

    fn principal(&self, subject: &str, _scopes: &[Arc<str>]) -> String {
        subject.to_owned()
    }
}

// ─── Claim mapping ────────────────────────────────────────────────────────────

#[test]
fn claims_survive_the_mapping() -> Result<(), Failure> {
    let claims: JwtClaims = serde_json::from_value(claims_json())?;
    let mapped = claims.into_token_claims();

    assert_eq!(mapped.subject(), Some("alice"));
    assert_eq!(mapped.issuer(), Some("https://auth.example"));
    assert_eq!(scope_strings(&mapped), ["mcp:tools"]);
    assert_eq!(mapped.audience().len(), 1);
    Ok(())
}

#[test]
fn an_audience_array_is_kept_element_by_element() -> Result<(), Failure> {
    // RFC 7519 §4.1.3 allows a string or an array, and `verify` matches any
    // element, so all of them have to survive the mapping.
    let claims: JwtClaims = serde_json::from_value(json!({
        "sub": "alice",
        "aud": ["https://mcp.example/mcp", "https://other.example/mcp"],
    }))?;
    assert_eq!(claims.into_token_claims().audience().len(), 2);
    Ok(())
}

#[test]
fn an_audience_that_is_not_a_uri_is_kept_rather_than_dropped() -> Result<(), Failure> {
    // This asserts the opposite of what it used to, and the reason is issue #46.
    //
    // Entries that did not parse as an RFC 9728 resource identifier were
    // dropped here, on the reasoning that an `aud` entry that is not an
    // absolute URI cannot equal this server's resource. That reasoning assumed
    // `aud` is always a URI, and it is not: RFC 7519 §4.1.3 defines it as a
    // "StringOrURI", and Microsoft Entra's v2.0 tokens carry a bare client-ID
    // GUID. Dropping the entry left such a token with *no* audience, so it was
    // refused whatever the policy said — and refusing for a mismatched
    // audience is a claim about the token, not about the decode that discarded
    // the answer.
    let claims: JwtClaims = serde_json::from_value(json!({
        "sub": "alice",
        "aud": ["not a uri", "https://mcp.example/mcp"],
    }))?;
    assert_eq!(claims.into_token_claims().audience().len(), 2);

    // And the one that matters: a token whose only audience is a bare GUID
    // carries it through, so a deployment can configure and match it.
    let guid = "8f7c1e42-0b3a-4f5d-9c6e-1a2b3c4d5e6f";
    let claims: JwtClaims = serde_json::from_value(json!({
        "sub": "alice",
        "aud": guid,
    }))?;
    let mapped = claims.into_token_claims();
    assert_eq!(mapped.audience().len(), 1);
    assert!(mapped.is_audience_str(guid));

    // An empty audience is still empty — this is about retention, not about
    // inventing an entry for a token that named none.
    let claims: JwtClaims = serde_json::from_value(json!({ "sub": "alice" }))?;
    assert!(claims.into_token_claims().audience().is_empty());
    Ok(())
}

#[test]
fn scopes_are_read_from_both_specified_spellings() -> Result<(), Failure> {
    // RFC 6749 §3.3 delimits with a space; RFC 9068 §2.2.3 sends an array.
    let delimited: JwtClaims = serde_json::from_value(json!({ "scope": "a b  c" }))?;
    assert_eq!(
        scope_strings(&delimited.into_token_claims()),
        ["a", "b", "c"]
    );

    let listed: JwtClaims = serde_json::from_value(json!({ "scope": ["a", "b"] }))?;
    assert_eq!(scope_strings(&listed.into_token_claims()), ["a", "b"]);
    Ok(())
}

#[test]
fn a_tab_in_scope_is_not_a_separator() -> Result<(), Failure> {
    // RFC 6749's ABNF admits %x20 and nothing else, so a tab belongs to a scope
    // name rather than separating two. Splitting on whitespace would hand a
    // deployment's policy two scope strings nobody issued.
    let claims: JwtClaims = serde_json::from_value(json!({ "scope": "a\tb" }))?;
    assert_eq!(scope_strings(&claims.into_token_claims()), ["a\tb"]);
    Ok(())
}

#[test]
fn an_empty_scope_is_not_a_scope() -> Result<(), Failure> {
    let claims: JwtClaims = serde_json::from_value(json!({ "scope": "" }))?;
    assert!(claims.into_token_claims().scopes().is_empty());
    Ok(())
}

#[test]
fn a_numeric_date_is_read_as_seconds_since_the_epoch() {
    // RFC 7519 §3.1 permits a non-integer, and the fraction is kept: a sender
    // writing `.5` granted half a second past the whole second, and truncating
    // would read that token as expired.
    assert_eq!(
        instant(1_700_000_060.5),
        Some(UNIX_EPOCH + Duration::from_millis(1_700_000_060_500))
    );
    assert_eq!(
        instant(1_700_000_060.0),
        Some(UNIX_EPOCH + Duration::from_secs(1_700_000_060))
    );
}

#[test]
fn a_numeric_date_that_is_not_a_date_is_dropped() {
    // Each of these is a value a peer can put in `exp` that is not a moment in
    // time. Dropping is the fail-secure answer for all three: `TokenClaims`
    // treats an absent expiry as unbounded, which `verify` and a deployment
    // decide about — mapping a nonsense value to some invented instant would
    // not.
    for seconds in [-1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(instant(seconds), None, "{seconds} must not become a date");
    }
}

#[test]
fn a_non_numeric_date_makes_the_payload_unreadable() {
    // Not a mapping rule but a parse rule, and worth stating because the
    // alternative — coercing a string to a date — would mean inventing an
    // expiry nobody issued. The token is refused before any key is consulted.
    let parsed = serde_json::from_value::<JwtClaims>(json!({ "exp": "not a number" }));
    assert!(parsed.is_err(), "a string must not become a NumericDate");
}

// ─── Key selection ────────────────────────────────────────────────────────────

#[test]
fn a_matching_key_is_selected() -> Result<(), Failure> {
    let keys = published()?;
    let chosen = select(&keys, &header(Algorithm::HS256))?;
    assert_eq!(chosen.common.key_id.as_deref(), Some(KID));
    Ok(())
}

#[test]
fn an_unknown_kid_is_refused() -> Result<(), Failure> {
    let keys = published()?;
    let mut header = header(Algorithm::HS256);
    header.kid = Some("retired".to_owned());
    assert_eq!(select(&keys, &header).err(), Some(KeyError::UnknownKey));
    Ok(())
}

#[test]
fn a_missing_kid_is_ambiguous_when_the_set_holds_more_than_one_key() -> Result<(), Failure> {
    let keys = set(vec![
        json!({ "kty": "oct", "kid": "one", "k": secret_b64() }),
        json!({ "kty": "oct", "kid": "two", "k": secret_b64() }),
    ])?;
    let mut header = header(Algorithm::HS256);
    header.kid = None;
    assert_eq!(select(&keys, &header).err(), Some(KeyError::NoKeyId));
    Ok(())
}

#[test]
fn a_missing_kid_is_unambiguous_for_a_single_key() -> Result<(), Failure> {
    // RFC 7517 §4.5 calls `kid` a hint, not a requirement.
    let keys = published()?;
    let mut header = header(Algorithm::HS256);
    header.kid = None;
    let chosen = select(&keys, &header)?;
    assert_eq!(chosen.common.key_id.as_deref(), Some(KID));
    Ok(())
}

#[test]
fn a_key_published_for_encryption_is_refused() -> Result<(), Failure> {
    let keys = set(vec![
        json!({ "kty": "oct", "kid": KID, "alg": "HS256", "k": secret_b64(), "use": "enc" }),
    ])?;
    assert_eq!(
        select(&keys, &header(Algorithm::HS256)).err(),
        Some(KeyError::NotForSignatures)
    );
    Ok(())
}

#[test]
fn a_key_that_may_not_verify_is_refused() -> Result<(), Failure> {
    let keys = set(vec![
        json!({ "kty": "oct", "kid": KID, "alg": "HS256", "k": secret_b64(), "key_ops": ["encrypt"] }),
    ])?;
    assert_eq!(
        select(&keys, &header(Algorithm::HS256)).err(),
        Some(KeyError::NotForVerification)
    );
    Ok(())
}

#[test]
fn a_key_published_for_another_algorithm_is_refused() -> Result<(), Failure> {
    // The rule that closes confusion at the selection layer: a key declaring
    // RS256 cannot be handed to an RS512 token, and — the case that matters —
    // cannot be handed to an HMAC token either.
    let keys = set(vec![
        json!({ "kty": "oct", "kid": KID, "alg": "RS256", "k": secret_b64() }),
    ])?;
    assert_eq!(
        select(&keys, &header(Algorithm::HS256)).err(),
        Some(KeyError::AlgorithmMismatch)
    );
    Ok(())
}

/// An RSA key with real, if unremarkable, parameters.
///
/// Nothing signs with it, because reaching the rejection must not depend on a
/// private key.
fn rsa_published() -> Result<JwkSet, Failure> {
    set(vec![json!({
        "kty": "RSA",
        "kid": KID,
        "n": "sXchDaQebHnPiGvyDOAT4saGEUetSyo9MKLOoWFsueri23bOdgWp4Dy1WlUzewbgBHod5pcM9H95GQRV3JDXboIRROSBigeC5yjU1hGzHHyXss8UDprecbAYxknTcQkhslANGRUZmdTOQ5qTRsLAt6BTYuyvVRdhS8exSZEy_c4gs_7svlJJQ4H9_NxsiIoLwAEk7-Q3UXERGYw_75IDrGA84-lA_-Ct4eTlXHBIY2EaV7t7LjJaynVJCpkv4LKjTTAumiGUIuQhrNhZLuF_RJLqHpM2kgWFLU7-VTdL1VbC2tejvcI2BlMkEpk1BzBZI0KQB0GaDWFLN-aEAw3vRw",
        "e": "AQAB",
    })])
}

#[test]
fn a_key_that_declares_no_algorithm_still_passes_selection() -> Result<(), Failure> {
    // The gap the family check exists to close. Selection checks only what the
    // key *says*: the `kid` matches, `use` and `key_ops` are absent, and no
    // `alg` is declared. Nothing here objects to an RSA key being handed to an
    // HMAC token — which is correct, because the answer depends on the key's
    // type, and that is `decode`'s job rather than selection's.
    let keys = rsa_published()?;
    let chosen = select(&keys, &header(Algorithm::HS256))?;
    assert_eq!(chosen.common.key_id.as_deref(), Some(KID));
    Ok(())
}

// ─── The source, end to end ───────────────────────────────────────────────────

#[tokio::test]
async fn a_correctly_signed_token_produces_claims() -> Result<(), Failure> {
    let source = source_over(published()?, vec![Algorithm::HS256]);
    let claims = source.claims(&signed(&claims_json())?).await?;
    assert_eq!(claims.subject(), Some("alice"));
    assert_eq!(scope_strings(&claims), ["mcp:tools"]);
    Ok(())
}

#[tokio::test]
async fn the_claims_reach_verify_and_produce_an_auth_context() -> Result<(), Failure> {
    use crate::application::oauth::{TokenPolicy, verify};
    use crate::domain::ProtocolVersion;
    use crate::domain::authorization_model;
    use crate::domain::mcp::ResourceUri;

    let source = source_over(published()?, vec![Algorithm::HS256]);
    let claims = source.claims(&signed(&claims_json())?).await?;

    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let resource = ResourceUri::new("https://mcp.example/mcp")?;
    // 2025-06-18 is the first revision that classifies the server as a resource
    // server, so it is the first whose tokens carry an audience to check.
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2025_06_18));
    let context = verify(&claims, &policy, &Permissive, now)?;

    assert_eq!(context.principal(), "alice");
    assert!(context.holds(ScopeSet::empty().with(Scope::Read)));
    Ok(())
}

#[tokio::test]
async fn a_tampered_payload_is_refused() -> Result<(), Failure> {
    // Re-sign different claims and splice their payload in, leaving the
    // signature over the original bytes. This is what an attacker presenting a
    // borrowed token does.
    let source = source_over(published()?, vec![Algorithm::HS256]);
    let original = signed(&claims_json())?;
    let borrowed = signed(&json!({
        "sub": "root",
        "aud": "https://mcp.example/mcp",
        "scope": "mcp:tools",
    }))?;

    let (head, _original_payload, signature) = segments_of(&original)?;
    let spliced = format!("{head}.{}.{signature}", payload_of(&borrowed)?);

    assert!(matches!(
        source.claims(&spliced).await,
        Err(SourceError::Refused(Refusal::Malformed))
    ));
    Ok(())
}

#[tokio::test]
async fn a_token_naming_an_unpublished_key_is_refused() -> Result<(), Failure> {
    let source = source_over(published()?, vec![Algorithm::HS256]);
    assert!(matches!(
        source
            .claims(&signed_named(&claims_json(), "retired")?)
            .await,
        Err(SourceError::Refused(Refusal::Unverified))
    ));
    Ok(())
}

#[tokio::test]
async fn garbage_is_refused_as_malformed() -> Result<(), Failure> {
    let source = source_over(published()?, vec![Algorithm::HS256]);
    assert!(matches!(
        source.claims("not-a-token").await,
        Err(SourceError::Refused(Refusal::Malformed))
    ));
    Ok(())
}

#[tokio::test]
async fn an_unsecured_alg_none_token_is_refused() -> Result<(), Failure> {
    // RFC 8725 §3.1. An unsecured JWS has an empty signature, so anything that
    // accepted one would accept a token nobody signed. No encoder produces this,
    // which is why it has to be assembled by hand.
    let source = source_over(published()?, vec![Algorithm::HS256]);
    let header = base64url(
        json!({ "alg": "none", "typ": "JWT" })
            .to_string()
            .as_bytes(),
    );
    let payload = base64url(claims_json().to_string().as_bytes());
    let token = format!("{header}.{payload}.");

    assert!(matches!(
        source.claims(&token).await,
        Err(SourceError::Refused(Refusal::Malformed))
    ));
    Ok(())
}

#[tokio::test]
async fn a_token_carrying_its_own_key_is_refused() -> Result<(), Failure> {
    // RFC 7515 §4.1.3. Honouring the embedded `jwk` would make verification a
    // fixed point: an attacker signs with a key they generated, embeds it, and
    // this server verifies against the attacker's own key.
    let keys = published()?;
    let own = keys
        .keys
        .first()
        .cloned()
        .ok_or(Failure::Key(KeyError::UnknownKey))?;
    let source = source_over(keys, vec![Algorithm::HS256]);

    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(KID.to_owned());
    header.jwk = Some(own);
    let token = encode(&header, &claims_json(), &EncodingKey::from_secret(SECRET))?;

    assert!(matches!(
        source.claims(&token).await,
        Err(SourceError::Refused(Refusal::Malformed))
    ));
    Ok(())
}

#[tokio::test]
async fn a_token_with_a_crit_header_is_refused() -> Result<(), Failure> {
    // RFC 7515 §4.1.11: a `crit` naming an extension the recipient does not
    // implement makes the JWS invalid. `jsonwebtoken` parses the field without
    // acting on it, so nothing downstream would catch this.
    let source = source_over(published()?, vec![Algorithm::HS256]);
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(KID.to_owned());
    header.crit = Some(vec!["urn:example:not-understood".to_owned()]);
    let token = encode(&header, &claims_json(), &EncodingKey::from_secret(SECRET))?;

    assert!(matches!(
        source.claims(&token).await,
        Err(SourceError::Refused(Refusal::Malformed))
    ));
    Ok(())
}

#[tokio::test]
async fn a_token_with_no_expiry_is_handed_to_verify_rather_than_refused_here() -> Result<(), Failure>
{
    // `required_spec_claims` is cleared so this source and the introspection
    // source agree that a token without `exp` is `verify`'s business rather than
    // a decode-time rejection. If that ever stops being true the two sources
    // disagree about the same token and only one of them says so.
    let source = source_over(published()?, vec![Algorithm::HS256]);
    let claims = json!({ "sub": "alice", "aud": "https://mcp.example/mcp", "scope": "mcp:tools" });
    assert!(source.claims(&signed(&claims)?).await.is_ok());
    Ok(())
}

#[tokio::test]
async fn an_unaccepted_algorithm_is_refused() -> Result<(), Failure> {
    // The source accepts RS256 only, the published set holds an HMAC key, and
    // the token is well-formed and correctly signed. It is refused because of
    // what this deployment will verify, which is the point of the accept-list.
    let source = source_over(published()?, default_algorithms());
    assert!(matches!(
        source.claims(&signed(&claims_json())?).await,
        Err(SourceError::Refused(Refusal::Unverified))
    ));
    Ok(())
}

#[tokio::test]
async fn an_unreachable_key_endpoint_is_unavailable_not_refused() -> Result<(), Failure> {
    // The distinction the error split exists for: nothing was decided about the
    // token, so a refusal would send the caller to re-authenticate against an
    // authorization server that is merely unreachable.
    //
    // Two things have to line up for the fetch to even be attempted, and both
    // were wrong in earlier drafts of this test. The token must be a real one,
    // because `claims` parses the JWS header before it fetches anything; and the
    // source must accept its algorithm, or `key_for` refuses on the accept-list
    // before it ever opens a socket.
    let source = JwtSource::new("http://127.0.0.1:1/jwks.json", vec![Algorithm::HS256]);
    assert!(matches!(
        source.claims(&signed(&claims_json())?).await,
        Err(SourceError::Unavailable(_))
    ));
    Ok(())
}

#[tokio::test]
async fn an_rsa_key_cannot_verify_an_hmac_token() -> Result<(), Failure> {
    // The algorithm-confusion attack, asserted at the boundary rather than at a
    // helper. A token signed with HMAC is presented, the authorization server
    // publishes only an RSA key, and the `kid` matches — so the only thing left
    // to catch it is the family check between the key's type and the header's
    // `alg`. Testing `select` alone would pass even with that check deleted.
    let source = source_over(rsa_published()?, vec![Algorithm::HS256]);
    assert!(matches!(
        source.claims(&signed(&claims_json())?).await,
        Err(SourceError::Refused(Refusal::Unverified))
    ));
    Ok(())
}

#[test]
fn no_error_this_source_produces_contains_the_token() -> Result<(), Failure> {
    // The test that matters most and is easiest to regress: a bearer credential
    // reaching a log through a `Display` impl is a credential that has to be
    // rotated.
    let token = signed(&claims_json())?;
    for refusal in [
        Refusal::Malformed,
        Refusal::Unverified,
        Refusal::AudienceMismatch,
    ] {
        let rendered = SourceError::from(refusal).to_string();
        assert!(
            !rendered.contains(&token),
            "error text leaked the token: {rendered}"
        );
    }
    Ok(())
}

/// The three segments of a JWS, in order.
///
/// Patterned rather than indexed throughout, because a token that is not three
/// segments is exactly the thing these tests are about and `parts[1]` would
/// panic on it instead of failing.
fn segments_of(token: &str) -> Result<(&str, &str, &str), Failure> {
    let mut segments = token.splitn(3, '.');
    match (segments.next(), segments.next(), segments.next()) {
        (Some(header), Some(payload), Some(signature)) => Ok((header, payload, signature)),
        _ => Err(Failure::Key(KeyError::UnknownKey)),
    }
}

/// The payload segment of a JWS, for splicing a tampered token together.
fn payload_of(token: &str) -> Result<&str, Failure> {
    segments_of(token).map(|(_, payload, _)| payload)
}

/// RFC 7515 §2: base64url without padding.
fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
