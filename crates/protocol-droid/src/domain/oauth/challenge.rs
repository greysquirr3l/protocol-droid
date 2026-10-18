//! The `Bearer` challenge an MCP server answers `401` and `403` with.
//!
//! RFC 6750 §3 puts the challenge in `WWW-Authenticate` as RFC 7235
//! auth-params, and RFC 9728 §5.1 adds `resource_metadata`. MCP reads three of
//! them: `resource_metadata` (where to discover the authorization server),
//! `scope` (what the current operation needs — "Clients MUST treat the scopes
//! provided in the challenge as authoritative for the current operation") and
//! `error` (`insufficient_scope` is what a step-up answers).
//! <https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization#scope-selection-strategy>
//!
//! The header may carry several challenges for several schemes; only the
//! `Bearer` one is read, and the scheme name is compared case-insensitively
//! as RFC 7235 §2.1 requires.

/// The parameters of one `Bearer` challenge that MCP acts on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BearerChallenge {
    /// Where the protected resource metadata is published.
    pub resource_metadata: Option<String>,
    /// The space-separated scopes the current operation needs.
    pub scope: Option<String>,
    /// The RFC 6750 §3.1 error code, such as `insufficient_scope`.
    pub error: Option<String>,
}

impl BearerChallenge {
    /// The challenged scopes, one per entry.
    #[must_use]
    pub fn scopes(&self) -> Vec<&str> {
        self.scope
            .as_deref()
            .map(|scope| scope.split_ascii_whitespace().collect())
            .unwrap_or_default()
    }

    /// Whether this is RFC 6750's `insufficient_scope`.
    #[must_use]
    pub fn is_insufficient_scope(&self) -> bool {
        self.error.as_deref() == Some("insufficient_scope")
    }
}

/// Reads the `Bearer` challenge out of a `WWW-Authenticate` value.
///
/// `None` when the header carries no `Bearer` challenge at all. A `Bearer`
/// challenge with no parameters is `Some` with every member empty: the server
/// said a bearer token is needed and nothing more.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::oauth::challenge::parse_bearer;
///
/// let challenge = parse_bearer(
///     r#"Bearer resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", scope="files:read files:write""#,
/// ).ok_or("no challenge")?;
/// assert_eq!(challenge.scopes(), ["files:read", "files:write"]);
/// # Ok::<(), &str>(())
/// ```
#[must_use]
pub fn parse_bearer(header: &str) -> Option<BearerChallenge> {
    let mut rest = header.trim();
    loop {
        let (scheme, after) = split_token(rest)?;
        let (params, next) = read_params(after);
        if scheme.eq_ignore_ascii_case("bearer") {
            let mut challenge = BearerChallenge::default();
            for (name, value) in params {
                match name.to_ascii_lowercase().as_str() {
                    "resource_metadata" => challenge.resource_metadata = Some(value),
                    "scope" => challenge.scope = Some(value),
                    "error" => challenge.error = Some(value),
                    _ => {}
                }
            }
            return Some(challenge);
        }
        rest = next?;
    }
}

/// Splits a leading token off `text`.
fn split_token(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start_matches([' ', '\t', ',']);
    let end = text
        .find([' ', '\t', ','])
        .unwrap_or(text.len());
    let (token, rest) = text.split_at(end);
    (!token.is_empty()).then_some((token, rest))
}

/// Reads `name=value` pairs until the next challenge or the end.
///
/// Returns the pairs and, when another challenge follows, the text it starts
/// at. A token not followed by `=` begins the next challenge.
fn read_params(text: &str) -> (Vec<(String, String)>, Option<&str>) {
    let mut params = Vec::new();
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start_matches([' ', '\t', ',']);
        if trimmed.is_empty() {
            return (params, None);
        }
        let name_end = trimmed
            .find(['=', ' ', '\t', ','])
            .unwrap_or(trimmed.len());
        let (name, after_name) = trimmed.split_at(name_end);
        let after_name_trimmed = after_name.trim_start();
        let Some(after_equals) = after_name_trimmed.strip_prefix('=') else {
            // No `=`: this token is the next challenge's scheme.
            return (params, Some(trimmed));
        };
        let after_equals = after_equals.trim_start();
        let (value, remaining) = after_equals.strip_prefix('"').map_or_else(
            || {
                let end = after_equals.find([',', ' ', '\t']).unwrap_or(after_equals.len());
                let (value, remaining) = after_equals.split_at(end);
                (value.to_owned(), remaining)
            },
            read_quoted,
        );
        params.push((name.to_owned(), value));
        rest = remaining;
    }
}

/// Reads an RFC 7230 quoted-string body, unescaping `\x`.
fn read_quoted(text: &str) -> (String, &str) {
    let mut value = String::new();
    let mut escaped = false;
    for (index, c) in text.char_indices() {
        if escaped {
            value.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            let after = text.get(index + 1..).unwrap_or_default();
            return (value, after);
        } else {
            value.push(c);
        }
    }
    (value, "")
}

#[cfg(test)]
mod tests {
    use super::parse_bearer;

    #[test]
    fn reads_the_three_parameters_mcp_uses() {
        let challenge = parse_bearer(
            r#"Bearer error="insufficient_scope", scope="files:write", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", error_description="needs write""#,
        );
        let challenge = challenge.unwrap_or_default();
        assert!(challenge.is_insufficient_scope());
        assert_eq!(challenge.scopes(), ["files:write"]);
        assert_eq!(
            challenge.resource_metadata.as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
    }

    #[test]
    fn finds_bearer_among_other_schemes() {
        let challenge = parse_bearer(r#"Basic realm="x", Bearer scope="a b""#).unwrap_or_default();
        assert_eq!(challenge.scopes(), ["a", "b"]);
    }

    #[test]
    fn the_scheme_is_case_insensitive_and_params_may_be_bare() {
        let challenge = parse_bearer("bearer error=invalid_token").unwrap_or_default();
        assert_eq!(challenge.error.as_deref(), Some("invalid_token"));
    }

    #[test]
    fn a_bare_bearer_is_a_challenge_with_nothing_in_it() {
        assert_eq!(parse_bearer("Bearer"), Some(super::BearerChallenge::default()));
    }

    #[test]
    fn no_bearer_is_none() {
        assert_eq!(parse_bearer(r#"Basic realm="x""#), None);
        assert_eq!(parse_bearer(""), None);
    }

    #[test]
    fn quoted_values_unescape() {
        let challenge = parse_bearer(r#"Bearer scope="a \"b\"""#).unwrap_or_default();
        assert_eq!(challenge.scope.as_deref(), Some(r#"a "b""#));
    }
}
