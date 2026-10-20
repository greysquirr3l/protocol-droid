//! The canonical resource identifier and the document that publishes it.
//!
//! RFC 9728 ("OAuth 2.0 Protected Resource Metadata") is what makes an MCP
//! server discoverable as an OAuth resource server: a server publishes a
//! metadata document naming its authorization servers, and a client that gets
//! a `401` reads it to find out where to get a token.
//!
//! That document's `resource` field *is* the audience. RFC 8707 resource
//! indicators bind a token to it, and this crate refuses a token whose audience
//! is not exactly this string. So [`ResourceUri`] is a value object rather than
//! a convenience: two spellings of the same server that compare unequal is a
//! token rejected for a reason nobody can see.
//!
//! # Where this lives
//!
//! `domain/`, because the canonicalisation rules are the security-relevant
//! part and an adapter should not get to choose them. Which *revisions* require
//! any of this is `authorization_model`.

use std::sync::Arc;

use core::fmt;
use core::str::FromStr;

use serde::{Serialize, Serializer};

/// The well-known path an MCP server publishes its metadata at.
///
/// RFC 9728 §3.1: the metadata for a resource at the origin root is
/// `/.well-known/oauth-protected-resource`, and the metadata for a resource at
/// some other path is `/.well-known/oauth-protected-resource` with that path
/// *appended*. So a server mounted at `/mcp` publishes at
/// `/.well-known/oauth-protected-resource/mcp` — see
/// [`ResourceUri::metadata_path`].
pub const WELL_KNOWN: &str = "/.well-known/oauth-protected-resource";

/// How a bearer token reaches the server.
///
/// RFC 9728 §2: `bearer_methods_supported` names the mechanisms this resource
/// accepts. Only the `header` mechanism is defined by the RFC, and it is the
/// only one this crate reads a token from — the alternative in the spec's
/// discussion is the `Authorization` request body parameter, which MCP's
/// transports have no place for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BearerMethod {
    /// RFC 6750 §2.1, the `Authorization: Bearer` header.
    ///
    /// The wire spelling is lower case because that is what the RFC's
    /// `bearer_methods_supported` array contains and what a client's parser
    /// compares against.
    #[default]
    Header,
}

impl BearerMethod {
    /// The value as it appears in a `bearer_methods_supported` array.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
        }
    }
}

/// The canonical resource identifier for an MCP server: the `resource` field,
/// and the audience a token must carry.
///
/// # Canonicalisation
///
/// Two rules, both from RFC 9728 §2 and RFC 8707 §2, and both of which a
/// hand-rolled string comparison would get wrong:
///
/// - **No trailing slash.** `https://example.com/mcp` and
///   `https://example.com/mcp/` are the same resource to a human and two
///   different strings to an `aud` comparison, so the slash is dropped once.
///   A bare origin (`https://example.com`) keeps none.
/// - **No default port.** `https://example.com:443/mcp` and
///   `https://example.com/mcp` are the same resource, so `443` and `80` are
///   dropped for their schemes.
///
/// Scheme and host are **not** case-folded beyond parsing the scheme, because
/// the host part is not something this crate can safely normalise: IDNA
/// punycode and case-sensitive comparison are a much larger problem than
/// authorization needs, and a wrong fold is a wrong audience. What is folded is
/// the scheme, which RFC 3986 §3.1 defines as case-insensitive.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::mcp::ResourceUri;
///
/// // The trailing slash is dropped, so both spellings name one resource.
/// assert_eq!(
///     ResourceUri::new("https://example.com/mcp/").unwrap().as_str(),
///     "https://example.com/mcp",
/// );
/// assert_eq!(
///     ResourceUri::new("https://example.com/mcp").unwrap(),
///     ResourceUri::new("https://example.com/mcp/").unwrap(),
/// );
///
/// // A default port is dropped too.
/// assert_eq!(
///     ResourceUri::new("HTTPS://example.com:443/mcp").unwrap().as_str(),
///     "https://example.com/mcp",
/// );
///
/// // A non-default port is part of the resource and is kept.
/// assert_eq!(
///     ResourceUri::new("https://example.com:8443/mcp").unwrap().as_str(),
///     "https://example.com:8443/mcp",
/// );
/// # Ok::<(), protocol_droid::domain::mcp::ResourceUriError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceUri(Arc<str>);

impl Serialize for ResourceUri {
    /// Serialises as the canonical string, which is the RFC 9728 `resource`
    /// value verbatim.
    ///
    /// Hand-written rather than derived because the inner `Arc<str>` is a
    /// newtype's implementation detail — a derived impl would make the
    /// canonical form load-bearing in a second place, and the whole point of
    /// the type is that there is exactly one spelling of a resource.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl ResourceUri {
    /// Canonicalises a resource identifier.
    ///
    /// The inherent twin of [`FromStr`], so a caller reading this module does
    /// not have to know that the parsing rule lives in a trait impl. Both do
    /// the same thing; `new` exists because the `parse` in a doctest is noise
    /// when the value came from a config file rather than from user input.
    ///
    /// # Errors
    ///
    /// The same as [`ResourceUri::from_str`].
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::mcp::ResourceUri;
    ///
    /// assert_eq!(
    ///     ResourceUri::new("https://example.com/mcp/")?.as_str(),
    ///     "https://example.com/mcp",
    /// );
    /// # Ok::<(), protocol_droid::domain::mcp::ResourceUriError>(())
    /// ```
    pub fn new(value: &str) -> Result<Self, ResourceUriError> {
        value.parse()
    }

    /// The canonical string, which is also the `resource` field and the
    /// expected `aud`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Builds a canonical identifier from a base origin and a mounted path.
    ///
    /// The join is done here rather than by the caller because the two halves
    /// each arrive from somewhere that gets slash handling wrong: a config file
    /// writes a trailing slash on the origin, and the mount point is an
    /// [`Endpoint`](crate::adapters::primary::transport::http::Endpoint) that already requires a leading one.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::new`] returns for the joined string.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::mcp::ResourceUri;
    ///
    /// let uri = ResourceUri::from_parts("https://example.com/", "/mcp")?;
    /// assert_eq!(uri.as_str(), "https://example.com/mcp");
    ///
    /// // An origin with no path still produces a usable identifier.
    /// let root = ResourceUri::from_parts("https://example.com", "")?;
    /// assert_eq!(root.as_str(), "https://example.com");
    /// # Ok::<(), protocol_droid::domain::mcp::ResourceUriError>(())
    /// ```
    pub fn from_parts(origin: &str, path: &str) -> Result<Self, ResourceUriError> {
        let base = origin.trim_end_matches('/');
        // A path of `/` means the resource *is* the origin; anything else is
        // appended to the origin with exactly one slash between them.
        let trimmed = path.trim_matches('/');
        let joined = if trimmed.is_empty() {
            base.to_owned()
        } else {
            format!("{base}/{trimmed}")
        };
        Self::new(&joined)
    }

    /// The RFC 9728 path this resource's metadata is published at.
    ///
    /// The resource's own path appended to [`WELL_KNOWN`], which is the
    /// path-inserted form from RFC 9728 §3.1. A resource at the origin root
    /// publishes at exactly [`WELL_KNOWN`].
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::mcp::ResourceUri;
    ///
    /// assert_eq!(
    ///     ResourceUri::new("https://example.com/mcp").unwrap().metadata_path(),
    ///     "/.well-known/oauth-protected-resource/mcp",
    /// );
    /// assert_eq!(
    ///     ResourceUri::new("https://example.com").unwrap().metadata_path(),
    ///     "/.well-known/oauth-protected-resource",
    /// );
    /// # Ok::<(), protocol_droid::domain::mcp::ResourceUriError>(())
    /// ```
    #[must_use]
    pub fn metadata_path(&self) -> String {
        let Some(path) = self.path() else {
            return WELL_KNOWN.to_owned();
        };
        format!("{WELL_KNOWN}{path}")
    }

    /// The absolute URL a `WWW-Authenticate` challenge points at.
    ///
    /// # Errors
    ///
    /// [`ResourceUriError::NoOrigin`] when this identifier has no origin, which
    /// [`Self::new`] rules out — so this cannot fire from a `ResourceUri` that
    /// was built through the constructor, and exists only so the return type is
    /// honest about the fallback rather than panicking.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::mcp::ResourceUri;
    ///
    /// let uri = ResourceUri::new("https://example.com/mcp")?;
    /// assert_eq!(
    ///     uri.metadata_url()?,
    ///     "https://example.com/.well-known/oauth-protected-resource/mcp",
    /// );
    /// # Ok::<(), protocol_droid::domain::mcp::ResourceUriError>(())
    /// ```
    pub fn metadata_url(&self) -> Result<String, ResourceUriError> {
        let origin = self.origin().ok_or(ResourceUriError::NoOrigin)?;
        Ok(format!("{origin}{}", self.metadata_path()))
    }

    /// The origin — scheme, host and any non-default port — with no path.
    fn origin(&self) -> Option<&str> {
        let after_scheme = self.0.find("://")? + 3;
        let end = self.0[after_scheme..]
            .find('/')
            .map_or(self.0.len(), |offset| after_scheme + offset);
        Some(&self.0[..end])
    }

    /// The path component, without the origin and without a trailing slash.
    fn path(&self) -> Option<&str> {
        let after_scheme = self.0.find("://")? + 3;
        let slash = self.0[after_scheme..].find('/')? + after_scheme;
        Some(&self.0[slash..])
    }
}

impl FromStr for ResourceUri {
    type Err = ResourceUriError;

    /// # Errors
    ///
    /// [`ResourceUriError::Empty`] for a blank value, and
    /// [`ResourceUriError::NotAbsolute`] when there is no `scheme://`. RFC 9728
    /// §2 requires `resource` to be an absolute URI, so a relative one is not
    /// a resource identifier and cannot be compared against an `aud`.
    ///
    /// A trailing slash and a default port are canonicalised rather than
    /// refused — see the type docs for why that is the right call.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::mcp::ResourceUri;
    ///
    /// assert!(ResourceUri::new("example.com/mcp").is_err());
    /// assert!(ResourceUri::new("  ").is_err());
    /// assert!(ResourceUri::new("https://example.com/mcp").is_ok());
    /// # Ok::<(), protocol_droid::domain::mcp::ResourceUriError>(())
    /// ```
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(ResourceUriError::Empty);
        }

        let Some(scheme_end) = trimmed.find("://") else {
            return Err(ResourceUriError::NotAbsolute);
        };
        let scheme = &trimmed[..scheme_end];
        if scheme.is_empty()
            || !scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
        {
            return Err(ResourceUriError::NotAbsolute);
        }

        // RFC 3986 §3.1: the scheme is case-insensitive. Only the scheme —
        // see the type docs on why the host is not folded.
        let mut canonical = String::with_capacity(trimmed.len());
        canonical.push_str(&scheme.to_ascii_lowercase());
        canonical.push_str("://");

        let rest = &trimmed[scheme_end + 3..];
        let (authority, path) = rest
            .find('/')
            .map_or((rest, ""), |slash| (&rest[..slash], &rest[slash..]));
        if authority.is_empty() {
            return Err(ResourceUriError::NoAuthority);
        }
        canonical.push_str(&drop_default_port(authority, scheme));
        // Exactly one trailing slash is dropped, and only from the end of a
        // non-empty path, so `/mcp//` keeps the second slash rather than
        // quietly collapsing a path that is genuinely different.
        let path = path.strip_suffix('/').unwrap_or(path);
        canonical.push_str(path);

        Ok(Self(Arc::from(canonical.as_str())))
    }
}

/// `authority` without its port when the port is the scheme's default.
///
/// Compares the scheme case-insensitively because the caller passes the raw
/// scheme, not the canonicalised one, and `HTTPS://example.com:443` has to
/// fold the same way `https://example.com:443` does.
///
/// The port separator is *not* always the last colon. In an IPv6 literal every
/// group is separated by one — `[::1]:8443` has three — so a `rsplit(':')` cuts
/// the host in half and then compares whatever followed to the default port.
/// The closing bracket is what marks the end of the host in that case, and
/// getting this wrong silently drops or keeps the port on an address nobody
/// writes as a hostname.
fn drop_default_port(authority: &str, scheme: &str) -> String {
    let default = match scheme.to_ascii_lowercase().as_str() {
        "https" => Some("443"),
        "http" => Some("80"),
        _ => None,
    };

    // For a bracketed literal the host is everything through the `]`, and the
    // separator is the first `:` after it. `find` on the tail returns an offset
    // *into the tail*, so the addition that steps past the bracket has to happen
    // inside the closure — chained after it, `close` is gone and only the
    // wrong-index number is left. For anything else the last `:` separates.
    let separator = if authority.starts_with('[') {
        authority.find(']').and_then(|close| {
            authority[close + 1..]
                .find(':')
                .map(|in_tail| close + 1 + in_tail)
        })
    } else {
        authority.rfind(':')
    };

    let Some(separator) = separator else {
        return authority.to_owned();
    };
    debug_assert!(authority.is_char_boundary(separator));
    let (host, port) = authority.split_at(separator);
    let port = &port[1..];

    if default == Some(port) {
        return host.to_owned();
    }
    authority.to_owned()
}

impl TryFrom<&str> for ResourceUri {
    type Error = ResourceUriError;

    /// # Errors
    ///
    /// The same as [`ResourceUri::new`].
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl fmt::Display for ResourceUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a string is not a usable resource identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ResourceUriError {
    /// The value was blank.
    #[error("the resource identifier is empty")]
    Empty,

    /// There was no `scheme://`, or the scheme was not a legal one.
    ///
    /// RFC 9728 §2 requires an absolute URI, so this is a refusal rather than
    /// something to guess at: a relative identifier cannot be compared against
    /// a token's `aud`, which is the only thing it is for.
    #[error("the resource identifier is not an absolute URI with an http(s) scheme")]
    NotAbsolute,

    /// The authority was empty between `://` and the first `/`.
    #[error("the resource identifier names no host")]
    NoAuthority,

    /// The derived metadata path was not usable.
    ///
    /// Unreachable through [`ResourceUri::new`] — the constructor refuses
    /// anything without an authority — so it exists to give
    /// [`ResourceUri::metadata_url`] an error type rather than a panic.
    #[error("the resource identifier has no origin to publish metadata under")]
    NoOrigin,
}

/// An RFC 9728 protected-resource metadata document.
///
/// What an MCP server publishes to tell a client which authorization servers can
/// issue tokens for it, and which scopes those tokens should carry.
///
/// # Wire form
///
/// Field names are the `snake_case` ones RFC 9728 §2 defines — unlike the rest of
/// MCP's wire types, so this one derives `Serialize` rather than renaming to
/// camelCase. `skip_serializing_if` is not an optimisation: §3.2 says a
/// parameter with zero values MUST be omitted, so an empty
/// `authorization_servers` is absent rather than `[]`. `resource` is the one
/// field that is always present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResourceMetadata {
    /// This server's canonical resource identifier. Required by RFC 9728 §2.
    resource: ResourceUri,

    /// Issuer URLs of the authorization servers that can issue tokens for this
    /// resource.
    ///
    /// A `Vec` rather than a set because RFC 9728 §2 defines an array and the
    /// order is not significant, but deduplication is still the server's
    /// decision — publishing the same issuer twice is odd rather than wrong,
    /// and refusing to publish it would be this type being clever with
    /// somebody's configuration.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    authorization_servers: Vec<Arc<str>>,

    /// Scopes a token for this resource should carry.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    scopes_supported: Vec<Arc<str>>,

    /// How a bearer token may be presented. Always [`BearerMethod::Header`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    bearer_methods_supported: Vec<BearerMethod>,

    /// Human-readable name for the resource, from RFC 9728 §2.
    #[serde(skip_serializing_if = "Option::is_none")]
    resource_name: Option<Arc<str>>,
}

impl ResourceMetadata {
    /// A document for `resource`, with the mandatory `resource` field and
    /// nothing optional.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::mcp::{ResourceMetadata, ResourceUri};
    ///
    /// let metadata = ResourceMetadata::new(ResourceUri::new("https://example.com/mcp")?);
    /// assert_eq!(metadata.resource().as_str(), "https://example.com/mcp");
    /// assert!(metadata.authorization_servers().is_empty());
    /// # Ok::<(), protocol_droid::domain::mcp::ResourceUriError>(())
    /// ```
    #[must_use]
    pub fn new(resource: ResourceUri) -> Self {
        Self {
            resource,
            authorization_servers: Vec::new(),
            scopes_supported: Vec::new(),
            bearer_methods_supported: vec![BearerMethod::Header],
            resource_name: None,
        }
    }

    /// Names an authorization server that can issue tokens for this resource.
    #[must_use]
    pub fn with_authorization_server(mut self, issuer: impl Into<Arc<str>>) -> Self {
        self.authorization_servers.push(issuer.into());
        self
    }

    /// Declares a scope a token may carry.
    #[must_use]
    pub fn with_scope(mut self, scope: impl Into<Arc<str>>) -> Self {
        self.scopes_supported.push(scope.into());
        self
    }

    /// Sets the human-readable name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<Arc<str>>) -> Self {
        self.resource_name = Some(name.into());
        self
    }

    /// The canonical resource identifier, and the audience a token must carry.
    #[must_use]
    pub const fn resource(&self) -> &ResourceUri {
        &self.resource
    }

    /// The authorization servers this resource names.
    #[must_use]
    pub fn authorization_servers(&self) -> &[Arc<str>] {
        &self.authorization_servers
    }

    /// The scopes this resource declares.
    #[must_use]
    pub fn scopes_supported(&self) -> &[Arc<str>] {
        &self.scopes_supported
    }

    /// How a bearer token may be presented.
    #[must_use]
    pub fn bearer_methods_supported(&self) -> &[BearerMethod] {
        &self.bearer_methods_supported
    }

    /// The `WWW-Authenticate` challenge for a request that was refused.
    ///
    /// RFC 9728 §5.1: the challenge carries `resource_metadata` so the client
    /// knows where to discover the authorization server, which is the whole
    /// reason a server publishes the document.
    ///
    /// # Errors
    ///
    /// [`ResourceUriError::NoOrigin`] when the resource identifier has no
    /// origin, which the constructor rules out.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::mcp::{ResourceMetadata, ResourceUri};
    ///
    /// let metadata = ResourceMetadata::new(ResourceUri::new("https://example.com/mcp")?);
    /// assert_eq!(
    ///     metadata.challenge()?,
    ///     "Bearer resource_metadata=\"https://example.com/.well-known/oauth-protected-resource/mcp\"",
    /// );
    /// # Ok::<(), protocol_droid::domain::mcp::ResourceUriError>(())
    /// ```
    pub fn challenge(&self) -> Result<String, ResourceUriError> {
        Ok(format!(
            "Bearer resource_metadata=\"{}\"",
            self.resource.metadata_url()?
        ))
    }
}

#[cfg(test)]
#[path = "resource_metadata/tests.rs"]
mod tests;

#[cfg(test)]
mod wire_tests {
    use super::{ResourceMetadata, ResourceUri, ResourceUriError};

    /// The fallible calls in this module answer unrelated error types, so the
    /// test returns a `Result` over this rather than unwrapping. Each variant
    /// says what went wrong, because a bare "test failed" hides which of the
    /// two steps it was.
    /// The first element of a JSON array parameter, or `None` when the value is
    /// absent or is not an array. Keeps the assertions from indexing.
    fn first_element(value: &serde_json::Value) -> Option<serde_json::Value> {
        value.as_array().and_then(|values| values.first().cloned())
    }

    impl core::fmt::Display for Failure {
        /// Reads both payloads, so a failure says *which* step broke rather
        /// than only that something did.
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Uri(e) => write!(f, "not a legal resource identifier: {e}"),
                Self::Json(e) => write!(f, "document did not serialise: {e}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    #[derive(Debug)]
    enum Failure {
        /// A literal in the test was not a legal resource identifier.
        Uri(ResourceUriError),
        /// The document did not serialise, which would be a bug in the derive.
        Json(serde_json::Error),
    }

    #[test]
    fn emits_rfc_9728_field_names_and_omits_empty_parameters() -> Result<(), Failure> {
        let full = ResourceMetadata::new(
            ResourceUri::new("https://example.com/mcp").map_err(Failure::Uri)?,
        )
        .with_authorization_server("https://auth.example.com")
        .with_scope("files:read")
        .with_name("Example");
        let full_json: serde_json::Value = serde_json::to_value(&full).map_err(Failure::Json)?;
        // `get` rather than indexing throughout: a wrong shape then fails an
        // assertion instead of panicking inside the test.
        let first = |key: &str| full_json.get(key).and_then(first_element);
        assert_eq!(
            full_json.get("resource"),
            Some(&serde_json::json!("https://example.com/mcp"))
        );
        assert_eq!(
            first("authorization_servers"),
            Some(serde_json::json!("https://auth.example.com"))
        );
        assert_eq!(
            first("scopes_supported"),
            Some(serde_json::json!("files:read"))
        );
        assert_eq!(
            first("bearer_methods_supported"),
            Some(serde_json::json!("header"))
        );
        assert_eq!(
            full_json.get("resource_name"),
            Some(&serde_json::json!("Example"))
        );

        let bare =
            ResourceMetadata::new(ResourceUri::new("https://example.com").map_err(Failure::Uri)?);
        let bare_json: serde_json::Value = serde_json::to_value(&bare).map_err(Failure::Json)?;
        assert_eq!(
            bare_json.get("resource"),
            Some(&serde_json::json!("https://example.com"))
        );
        for absent in ["authorization_servers", "scopes_supported", "resource_name"] {
            assert!(
                bare_json.get(absent).is_none(),
                "RFC 9728 3.2 requires {absent} be omitted when empty, got {bare_json}"
            );
        }
        // `bearer_methods_supported` is the exception: `new` defaults it to
        // `["header"]` because this crate reads the token from the
        // `Authorization` header and nowhere else, so it is never empty and
        // always published.
        assert_eq!(
            bare_json
                .get("bearer_methods_supported")
                .and_then(first_element),
            Some(serde_json::json!("header"))
        );
        Ok(())
    }
}
