//! Where a UTCP client is allowed to connect.
//!
//! # The rule the specification sets
//!
//! A UTCP client reaches three kinds of endpoint: the provider's manual, the
//! tools' own endpoints, and the `OAuth2` token endpoint. The specification
//! applies one rule to all three — **HTTPS, or plain HTTP to a literal
//! loopback address** — and adds two rules that only make sense once the
//! first is in place:
//!
//! - **A redirect is re-validated at every hop.** A hop that leaves the
//!   allowed set is refused, and a credential is not carried across an origin
//!   boundary.
//! - **Remote content does not get the loopback allowance.** A manual
//!   discovered from a non-loopback origin may not declare tools at loopback
//!   URLs. Otherwise a hostile provider could point a client at the machine
//!   the client runs on, and the loopback exception would wave it through.
//!
//! # Why the loopback set is wider than the crate's other one
//!
//! The crate already knows what "loopback" means for an inbound `Origin`
//! header ([`crate::adapters::primary::transport::origin`]), and that set is
//! deliberately narrow: `localhost`, `127.0.0.1`, `::1`. That check defends
//! against DNS rebinding, where the *name* must be compared whole.
//!
//! This check defends against a different thing: a URL in a document, where
//! the address is a literal and the question is which interface it names. The
//! specification's set is `localhost`, all of `127.0.0.0/8`, `::1`, and their
//! wildcard and IPv4-mapped forms, and this module implements that set. Two
//! different questions, two different answers, and reusing the narrow one
//! here would leave `127.0.0.2` — a real loopback address — refused.
//!
//! # What is *not* here
//!
//! DNS resolution. A hostname is not resolved here, so `127.0.0.1.nip.io`
//! is not loopback: it is a name, and a name is not a literal address. The
//! rule is about literals, and pretending otherwise would re-open the hole
//! the narrow inbound check exists to close.

use std::net::IpAddr;

#[cfg(test)]
mod tests;

/// Where a UTCP client may connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// An HTTPS URL to any host.
    Secure {
        /// The host, as the URL spells it.
        host: String,
    },
    /// A plain-HTTP URL to a literal loopback address.
    Loopback {
        /// The host, as the URL spells it.
        host: String,
    },
}

impl Endpoint {
    /// The host this endpoint names.
    #[must_use]
    pub fn host(&self) -> &str {
        match self {
            Self::Secure { host } | Self::Loopback { host } => host,
        }
    }

    /// Whether this endpoint is a loopback address.
    ///
    /// The distinction matters for the remote-content rule: a tool at a
    /// loopback URL is only reachable when the manual that declared it was
    /// itself discovered locally.
    #[must_use]
    pub const fn is_loopback(&self) -> bool {
        matches!(self, Self::Loopback { .. })
    }

    /// Whether the host is the same on both sides.
    ///
    /// Compared whole and case-insensitively, because a host is a name and
    /// `Example.COM` and `example.com` are the same host.
    #[must_use]
    pub fn same_host(&self, other: &Self) -> bool {
        self.host().eq_ignore_ascii_case(other.host())
    }

    /// Whether a redirect from this endpoint to `next` may be followed.
    ///
    /// The specification's rule: a redirect is never followed from a
    /// non-loopback origin into loopback. A remote server that can steer a
    /// request at the client's own services has won something it should not
    /// have, and the loopback exception would wave it through.
    ///
    /// A redirect *out* of loopback to a remote host is allowed. The content
    /// it reaches is remote, and the caller learns that from the endpoint it
    /// lands on — a loopback discovery that redirects to a remote origin is
    /// serving remote content and loses the allowance.
    ///
    /// # Errors
    ///
    /// [`EndpointError::RedirectIntoLoopback`] when a remote host redirects
    /// into loopback.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::utcp::endpoint::{Endpoint, classify};
    ///
    /// let remote = classify("https://api.example.com/utcp")?;
    /// let loopback = classify("http://127.0.0.1:3000/utcp")?;
    ///
    /// assert!(remote.may_redirect_to(&loopback).is_err());
    /// assert!(loopback.may_redirect_to(&remote).is_ok());
    /// # Ok::<(), protocol_droid::domain::utcp::endpoint::EndpointError>(())
    /// ```
    pub fn may_redirect_to(&self, next: &Self) -> Result<(), EndpointError> {
        if !self.is_loopback() && next.is_loopback() {
            return Err(EndpointError::RedirectIntoLoopback {
                origin: self.host().to_owned(),
                host: next.host().to_owned(),
            });
        }
        Ok(())
    }
}

/// Why a URL is not one this build will connect to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    /// The URL could not be parsed.
    #[error("`{url}` is not a URL: {detail}")]
    Unparseable {
        /// The URL as it was given.
        url: String,
        /// What the parser said.
        detail: String,
    },
    /// The scheme is not one this build speaks.
    #[error("`{url}` uses the `{scheme}` scheme, which is not one this build connects over")]
    UnsupportedScheme {
        /// The URL as it was given.
        url: String,
        /// The scheme it used.
        scheme: String,
    },
    /// The URL is plain HTTP to somewhere that is not loopback.
    #[error("`{url}` is plain HTTP to `{host}`, which is not a loopback address")]
    NotSecure {
        /// The URL as it was given.
        url: String,
        /// The host it names.
        host: String,
    },
    /// A remote manual declares a tool at a loopback URL.
    #[error("the manual was discovered at `{origin}`, so it may not declare a tool at the loopback address `{host}`")]
    RemoteDeclaresLoopback {
        /// Where the manual was discovered.
        origin: String,
        /// The loopback host the manual declared.
        host: String,
    },
    /// A remote host redirected into loopback.
    #[error("a redirect from `{origin}` to the loopback address `{host}` was not followed")]
    RedirectIntoLoopback {
        /// Where the redirect came from.
        origin: String,
        /// The loopback host it pointed at.
        host: String,
    },
}

/// The scheme HTTPS.
const HTTPS: &str = "https";
/// The scheme plain HTTP.
const HTTP: &str = "http";

/// Classifies a URL, refusing the ones the specification refuses.
///
/// # Errors
///
/// [`EndpointError::Unparseable`] when the text is not a URL,
/// [`EndpointError::UnsupportedScheme`] when the scheme is not HTTP or
/// HTTPS, and [`EndpointError::NotSecure`] when the URL is plain HTTP to a
/// host that is not a literal loopback address.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::utcp::endpoint::{Endpoint, classify};
///
/// let endpoint = classify("https://api.example.com/utcp")?;
/// assert!(!endpoint.is_loopback());
///
/// let endpoint = classify("http://127.0.0.1:3000/utcp")?;
/// assert!(endpoint.is_loopback());
///
/// assert!(classify("http://api.example.com/utcp").is_err());
/// # Ok::<(), protocol_droid::domain::utcp::endpoint::EndpointError>(())
/// ```
pub fn classify(url: &str) -> Result<Endpoint, EndpointError> {
    let parsed = url::Url::parse(url).map_err(|error| EndpointError::Unparseable {
        url: url.to_owned(),
        detail: error.to_string(),
    })?;
    classify_parsed(url, &parsed)
}

/// Classifies an already-parsed URL.
///
/// # Errors
///
/// As [`classify`].
pub fn classify_parsed(original: &str, parsed: &url::Url) -> Result<Endpoint, EndpointError> {
    let host = parsed.host_str().unwrap_or_default().to_owned();
    match parsed.scheme() {
        HTTPS => Ok(Endpoint::Secure { host }),
        HTTP => {
            if is_loopback_host(&host) {
                Ok(Endpoint::Loopback { host })
            } else {
                Err(EndpointError::NotSecure {
                    url: original.to_owned(),
                    host,
                })
            }
        }
        scheme => Err(EndpointError::UnsupportedScheme {
            url: original.to_owned(),
            scheme: scheme.to_owned(),
        }),
    }
}

/// Whether a host is a literal loopback address.
///
/// The set is the specification's: `localhost`, all of `127.0.0.0/8`, `::1`,
/// and the wildcard and IPv4-mapped forms of those.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::utcp::endpoint::is_loopback_host;
///
/// assert!(is_loopback_host("localhost"));
/// assert!(is_loopback_host("127.0.0.1"));
/// assert!(is_loopback_host("127.0.0.2"));
/// assert!(is_loopback_host("::1"));
/// assert!(is_loopback_host("::ffff:127.0.0.1"));
/// assert!(!is_loopback_host("example.com"));
/// assert!(!is_loopback_host("127.0.0.1.example.com"));
/// ```
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => address.is_loopback() || address.is_unspecified(),
        Ok(IpAddr::V6(address)) => {
            address.is_loopback()
                || address.is_unspecified()
                // An IPv4-mapped address names the IPv4 interface it wraps,
                // so `::ffff:127.0.0.1` reaches loopback exactly as
                // `127.0.0.1` does.
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback() || mapped.is_unspecified())
        }
        // A name is not an address. `127.0.0.1.nip.io` resolves to loopback
        // on some resolvers, and treating it as loopback here would be a
        // hole shaped exactly like the one this function exists to close.
        Err(_) => false,
    }
}

/// Whether a manual discovered at `origin` may declare a tool at `target`.
///
/// The specification's remote-content rule: the loopback allowance exists for
/// local development, and it is not extended to content a remote origin
/// served.
///
/// # Errors
///
/// [`EndpointError::RemoteDeclaresLoopback`] when `target` is a loopback
/// address and `origin` is not one.
pub fn check_declared(
    origin: &Endpoint,
    origin_url: &str,
    target: &Endpoint,
) -> Result<(), EndpointError> {
    if target.is_loopback() && !origin.is_loopback() {
        return Err(EndpointError::RemoteDeclaresLoopback {
            origin: origin_url.to_owned(),
            host: target.host().to_owned(),
        });
    }
    Ok(())
}
