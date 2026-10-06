//! Canonicalisation, metadata paths, and the challenge.
//!
//! Canonicalisation is the part that has to be exactly right: every one of
//! these tests is a spelling of a resource that a hand-written string compare
//! would treat as distinct from another, and each of them changes which tokens
//! are accepted.

use super::{BearerMethod, ResourceMetadata, ResourceUri, ResourceUriError, WELL_KNOWN};
use crate::domain::ProtocolVersion;
use crate::domain::mcp::authorization::authorization_model;

/// One error type for every test, so a failure names the literal that was
/// refused rather than merely reporting that something was.
type TestError = ResourceUriError;

/// The canonical form of `input`, or `None` when it is not a resource.
fn canonical(input: &str) -> Option<String> {
    ResourceUri::new(input)
        .ok()
        .map(|uri| uri.as_str().to_owned())
}

#[test]
fn a_trailing_slash_names_the_same_resource() {
    // The most consequential case: a server configured with
    // `https://example.com/mcp/` and a client that strips the slash would
    // otherwise disagree about the audience on every single request.
    for (bare, slashed) in [
        ("https://example.com/mcp", "https://example.com/mcp/"),
        ("https://example.com", "https://example.com/"),
        ("https://example.com/", "https://example.com"),
        ("https://example.com/a/b", "https://example.com/a/b/"),
    ] {
        assert_eq!(
            canonical(bare),
            canonical(slashed),
            "{bare} and {slashed} must be one resource",
        );
    }
}

#[test]
fn only_one_trailing_slash_is_dropped() {
    // `//` is a genuinely different path from `/`, and collapsing it would let a
    // client and a server agree on one audience for two different routes.
    assert_eq!(
        canonical("https://example.com/mcp//"),
        Some("https://example.com/mcp/".to_owned()),
        "a doubled trailing slash is a real path",
    );
}

#[test]
fn the_scheme_is_folded_but_the_host_is_not() {
    // RFC 3986 §3.1 makes the scheme case-insensitive. The host is left alone:
    // folding it wrongly is a wrong audience, and IDNA is not something this
    // crate should be guessing at.
    assert_eq!(
        canonical("HTTPS://example.com/mcp"),
        Some("https://example.com/mcp".to_owned()),
    );
    assert_eq!(
        canonical("HtTpS://EXAMPLE.COM/mcp"),
        Some("https://EXAMPLE.COM/mcp".to_owned()),
        "the host must not be case-folded",
    );
}

#[test]
fn a_default_port_is_not_part_of_the_resource() {
    for (input, expected) in [
        ("https://example.com:443/mcp", "https://example.com/mcp"),
        ("http://example.com:80/mcp", "http://example.com/mcp"),
        ("HTTPS://example.com:443/mcp", "https://example.com/mcp"),
    ] {
        assert_eq!(canonical(input), Some(expected.to_owned()), "{input}");
    }
}

#[test]
fn a_non_default_port_is_part_of_the_resource() {
    // Dropping these would merge two different servers into one audience.
    for input in [
        "https://example.com:8443/mcp",
        "http://example.com:8080/mcp",
        "https://example.com:80/mcp",
    ] {
        assert!(
            canonical(input).is_some_and(|uri| uri.contains(':')),
            "{input} lost its port",
        );
    }
    assert_ne!(
        canonical("https://example.com:8443/mcp"),
        canonical("https://example.com:8444/mcp"),
        "two ports are two resources",
    );
}

#[test]
fn an_ipv6_literal_survives_intact() {
    // The `[::1]` case. Every group in a literal is colon-separated, so an
    // `rsplit(':')` cuts the host in half and then compares whatever trailed to
    // the default port — which silently keeps `:443` on a resource whose
    // canonical form is the one without it.
    assert_eq!(
        canonical("https://[::1]:8443/mcp"),
        Some("https://[::1]:8443/mcp".to_owned()),
        "a non-default port beside a literal is kept",
    );
    assert_eq!(
        canonical("https://[::1]/mcp"),
        Some("https://[::1]/mcp".to_owned()),
    );
    assert_eq!(
        canonical("https://[::1]:443/mcp"),
        Some("https://[::1]/mcp".to_owned()),
        "a default port beside a literal is still dropped",
    );
}

#[test]
fn a_relative_identifier_is_refused() {
    // RFC 9728 §2 requires an absolute URI, and a relative one cannot be
    // compared against a token's `aud` — which is the only thing it is for.
    for input in [
        "",
        "   ",
        "example.com/mcp",
        "/mcp",
        "://example.com",
        "/ /mcp",
    ] {
        assert!(
            ResourceUri::new(input).is_err(),
            "{input:?} should not parse as a resource identifier",
        );
    }
}

#[test]
fn the_metadata_path_is_the_resource_path_appended() -> Result<(), TestError> {
    // RFC 9728 §3.1, path-inserted form.
    for (resource, expected) in [
        ("https://example.com", WELL_KNOWN),
        (
            "https://example.com/mcp",
            "/.well-known/oauth-protected-resource/mcp",
        ),
        (
            "https://example.com/a/b",
            "/.well-known/oauth-protected-resource/a/b",
        ),
    ] {
        let uri = ResourceUri::new(resource)?;
        assert_eq!(uri.metadata_path(), expected, "{resource}");
    }
    Ok(())
}

#[test]
fn the_metadata_url_is_under_the_origin_not_under_the_resource() -> Result<(), TestError> {
    // The distinction that matters: the document is published at the *origin*
    // with the resource's path inserted. Deriving it from the full identifier
    // instead would publish at `.../mcp/.well-known/...`, which no client looks
    // there for.
    let uri = ResourceUri::new("https://example.com:8443/a/mcp")?;
    assert_eq!(
        uri.metadata_url()?,
        "https://example.com:8443/.well-known/oauth-protected-resource/a/mcp",
    );
    Ok(())
}

#[test]
fn the_challenge_points_at_the_metadata_document() -> Result<(), TestError> {
    // RFC 9728 §5.1. A client that receives this knows where to look without
    // having been configured with the authorization server.
    let resource = ResourceUri::new("https://example.com/mcp")?;
    assert_eq!(
        ResourceMetadata::new(resource).challenge()?,
        "Bearer resource_metadata=\"https://example.com/.well-known/oauth-protected-resource/mcp\"",
    );
    Ok(())
}

#[test]
fn the_challenge_quotes_its_url() -> Result<(), TestError> {
    // An unquoted parameter value is a header a strict client cannot parse, and
    // the failure surfaces as "no authorization server found" rather than as a
    // parse error, so it is worth asserting directly.
    let resource = ResourceUri::new("https://example.com/mcp")?;
    let challenge = ResourceMetadata::new(resource).challenge()?;
    assert!(challenge.starts_with("Bearer "));
    assert!(challenge.contains("resource_metadata=\""));
    assert!(challenge.ends_with('"'));
    Ok(())
}

#[test]
fn a_metadata_document_defaults_to_the_header_mechanism() -> Result<(), TestError> {
    // RFC 9728 §2: `bearer_methods_supported` names the accepted mechanisms.
    // Always `header` here because it is the only one this crate reads a token
    // from — publishing another would invite a client to use a mechanism the
    // transport has nowhere to put.
    let resource = ResourceUri::new("https://example.com")?;
    let metadata = ResourceMetadata::new(resource);
    assert_eq!(metadata.bearer_methods_supported(), &[BearerMethod::Header]);
    assert_eq!(BearerMethod::Header.as_str(), "header");
    Ok(())
}

#[test]
fn a_metadata_document_starts_with_only_its_required_field() -> Result<(), TestError> {
    let resource = ResourceUri::new("https://example.com/mcp")?;
    let metadata = ResourceMetadata::new(resource);
    assert!(metadata.authorization_servers().is_empty());
    assert!(metadata.scopes_supported().is_empty());
    Ok(())
}

#[test]
fn the_document_builds_by_chaining() -> Result<(), TestError> {
    let resource = ResourceUri::new("https://example.com/mcp")?;
    let metadata = ResourceMetadata::new(resource)
        .with_name("Example")
        .with_authorization_server("https://auth.example.com")
        .with_scope("mcp:tools");
    assert_eq!(metadata.authorization_servers().len(), 1);
    assert_eq!(metadata.scopes_supported(), &["mcp:tools".into()]);
    Ok(())
}

#[test]
fn only_revisions_that_publish_metadata_are_eligible_to() {
    // The connection to the version table: `2025-06-18` changelog §3 (PR #338)
    // is where the resource-server classification and this document arrive.
    // `2025-03-26` has OAuth 2.1 but nothing to publish, and `2024-11-05` never
    // defined authorization at all — so a server publishing this document for
    // either would be inventing a contract.
    for version in ProtocolVersion::KNOWN {
        let publishes = authorization_model(version).protected_resource_metadata;
        assert_eq!(
            publishes,
            version >= ProtocolVersion::V2025_06_18,
            "{version} disagrees with the version table",
        );
    }
}

#[test]
fn from_parts_joins_without_doubling_slashes() -> Result<(), TestError> {
    // Both halves arrive from places that get this wrong: a config file writes
    // a trailing slash on the origin, a mount point requires a leading one.
    for (origin, path, expected) in [
        ("https://example.com/", "/mcp", "https://example.com/mcp"),
        ("https://example.com", "/mcp", "https://example.com/mcp"),
        ("https://example.com/", "mcp", "https://example.com/mcp"),
        (
            "https://example.com:8443",
            "/a/b",
            "https://example.com:8443/a/b",
        ),
        ("https://example.com", "/", "https://example.com"),
        ("https://example.com/", "", "https://example.com"),
    ] {
        let uri = ResourceUri::from_parts(origin, path)?;
        assert_eq!(uri.as_str(), expected, "{origin} + {path}");
    }
    Ok(())
}

#[test]
fn equality_means_canonically_identical_not_merely_similar() -> Result<(), TestError> {
    // The property every audience check rests on: canonicalisation happens on
    // the way in, so `==` between two `ResourceUri`s means the two strings are
    // byte-identical after canonicalisation rather than merely describing the
    // same server.
    let slashed = ResourceUri::new("https://example.com/mcp/")?;
    let plain = ResourceUri::new("https://example.com/mcp")?;
    assert_eq!(slashed, plain);
    assert_eq!(slashed.as_str(), plain.as_str());

    // Only the scheme folds. The host's case survives, so these are *not*
    // equal — asserted so the lines above are not read as "anything that looks
    // like the same host compares equal", which is a much weaker guarantee than
    // the audience check needs.
    let upper_host = ResourceUri::new("https://EXAMPLE.com/mcp")?;
    assert_ne!(slashed, upper_host);
    Ok(())
}
