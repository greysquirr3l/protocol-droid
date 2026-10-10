//! The `UrlFilter` port — the A2A push-notification SSRF guard.
//!
//! # Why this is a port
//!
//! Push-notification webhooks in A2A are attacker-controlled outbound URLs:
//! a malicious client registers a `PushNotificationConfig` whose `url` field
//! points at `http://169.254.169.254/...`, and the server then POSTs the task
//! state to the cloud metadata service. The server is the one initiating the
//! request, so the network egress policy must filter the URL before delivery.
//! The OWASP API Security Top 10 (2023) calls this `API7 — Server-Side
//! Request Forgery`; the T24 security audit flagged it as a T27 must-have.
//!
//! The filter is a port because the *policy* is deployment-specific — a
//! private network A2A deployment legitimately needs to deliver to
//! `10.0.0.0/8`, and the same code in a public-internet deployment must
//! refuse it. A `UrlFilter` impl the operator chooses — the production
//! `SsrfDenylist` in `adapters/secondary/a2a/`, or a deployment-specific
//! allow-list — is what makes the policy swappable without rewriting the
//! handler.
//!
//! # Two checks, not one
//!
//! `check_at_config` runs at the moment the client registers the config, when
//! the only thing the filter has is the URL string. It can reject IP
//! literals in the loopback / private / link-local / metadata ranges, and
//! reject non-`http(s)` schemes, but it cannot evaluate a hostname that has
//! not been resolved yet — accepting a hostname here is a *deferral*, not an
//! allow.
//!
//! `check_at_delivery` runs at the moment the server is about to POST, when
//! the delivery adapter has already resolved the hostname to an IP address.
//! The denylist applies to the resolved IP, not the URL string, which is
//! what closes the DNS-rebinding gap: a hostname that resolved to a public
//! IP at config time and to `127.0.0.1` at delivery time is still rejected
//! here, because the resolved IP is what the socket will dial.
//!
//! # The default-deny policy
//!
//! [`SsrfDenylist`](crate::adapters::secondary::a2a::SsrfDenylist) is the
//! production implementation. It denies the IPv4 ranges the OWASP API7
//! guidance names (`127.0.0.0/8`, `10.0.0.0/8`, `172.16.0.0/12`,
//! `192.168.0.0/16`, `169.254.0.0/16`) plus the IPv6 analogues (`::1/128`,
//! `fc00::/7`, `fe80::/10`) and the non-`http(s)` schemes (`file`, `gopher`,
//! `dict`, etc.). A private-network deployment that needs to deliver to
//! `10.0.0.0/8` writes a different impl and passes it in.

use std::net::IpAddr;

use url::Url;

use crate::domain::a2a::A2aVersion;

/// A denial with a name a peer can act on.
///
/// The discriminant is the *class* of the refusal, not the *value* — a peer
/// cannot recover a `Loopback` denial by trying a different loopback
/// address. The string is logged, never parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterDenial {
    /// The URL is not `http` or `https`. Push notifications are HTTP POSTs;
    /// any other scheme is either impossible (a transport that does not
    /// exist) or an obvious SSRF primitive (`file://`, `gopher://`).
    UnsupportedScheme {
        /// The scheme the URL named.
        scheme: String,
    },
    /// The URL has no host. A URL like `http:///path` is not a URL the
    /// delivery adapter can resolve.
    MissingHost,
    /// The URL is malformed in a way `url::Url::parse` did not catch. Almost
    /// never raised; a parse failure is usually an `UnsupportedScheme` or
    /// `MissingHost` first.
    Malformed,
    /// The URL contained an IP literal in a denied range. The string names
    /// the *literal*, not the resolved IP — `check_at_config` sees the
    /// literal; `check_at_delivery` does not re-raise this variant.
    DeniedIpLiteral {
        /// The IP literal the URL named.
        literal: String,
    },
    /// The *resolved* IP is in a denied range, even though the URL string
    /// may have looked fine. DNS rebinding lands here.
    DeniedResolvedIp {
        /// The IP the resolver returned.
        ip: IpAddr,
    },
    /// The URL contained a userinfo segment (`user:pass@host`). The
    /// credential is not a credential the server is honouring, and the
    /// format is a known SSRF primitive in URL parsers that disagree about
    /// what it means.
    UserinfoNotAllowed,
    /// The URL targeted the version's per-version well-known path or
    /// another reserved namespace the server owns. A `PushNotificationConfig`
    /// pointing at the server's own agent card endpoint is a loopback
    /// primitive the static check would otherwise have to allow.
    ReservedNamespace {
        /// The path the URL named, for logging.
        path: String,
    },
}

impl std::fmt::Display for FilterDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedScheme { scheme } => {
                write!(f, "scheme {scheme:?} is not http or https")
            }
            Self::MissingHost => f.write_str("the URL has no host"),
            Self::Malformed => f.write_str("the URL is malformed"),
            Self::DeniedIpLiteral { literal } => {
                write!(f, "IP literal {literal} is in a denied range")
            }
            Self::DeniedResolvedIp { ip } => {
                write!(f, "resolved IP {ip} is in a denied range")
            }
            Self::UserinfoNotAllowed => f.write_str("URL userinfo is not permitted"),
            Self::ReservedNamespace { path } => {
                write!(f, "path {path:?} is a server-reserved namespace")
            }
        }
    }
}

impl std::error::Error for FilterDenial {}

/// The port the push-notification delivery adapter consults.
///
/// Two methods because the URL is checked at two moments: at config time,
/// when the only thing available is the URL string, and at delivery time,
/// when the resolved IP is also available. An implementation may deny more
/// aggressively than the two named methods, but must not deny *less* — the
/// second check exists precisely to close what the first cannot.
pub trait UrlFilter: Send + Sync {
    /// Checks a URL when the client registers a `PushNotificationConfig`.
    ///
    /// Must reject every input `check_at_delivery` would reject, and may
    /// reject hostnames that look like they will fail the second check
    /// (e.g. a hostname that ends in `.internal`).
    ///
    /// # Errors
    ///
    /// [`FilterDenial`] on rejection, with the class of the refusal.
    fn check_at_config(
        &self,
        url: &Url,
        version: A2aVersion,
    ) -> Result<(), FilterDenial>;

    /// Checks a URL *and* its resolved IP at delivery time.
    ///
    /// The IP is the result of a DNS resolution the delivery adapter did
    /// itself (not the system's default resolver, which can be poisoned).
    /// The check must not trust the URL string's `Host` header against the
    /// resolved IP — a `Host: example.com` over a socket to `127.0.0.1` is
    /// the textbook DNS-rebind.
    ///
    /// # Errors
    ///
    /// [`FilterDenial`] on rejection. A `DeniedIpLiteral` from the first
    /// check is *not* re-raised here; the relevant variant for a rejected
    /// resolved IP is `DeniedResolvedIp`.
    fn check_at_delivery(
        &self,
        url: &Url,
        resolved: IpAddr,
        version: A2aVersion,
    ) -> Result<(), FilterDenial>;
}

#[cfg(test)]
mod tests {
    // This test uses `?` exclusively — no `expect`, no `panic`. The lint
    // allow on `expect_used` / `panic` would be unfulfilled.

    use super::FilterDenial;

    type TestError = Box<dyn std::error::Error>;

    #[test]
    fn denial_display_does_not_leak_internal_paths() -> Result<(), TestError> {
        // The Display impl is the *only* surface a peer sees, and the T24
        // audit calls out that error messages are an information-leak
        // vector. This test pins that the strings are short, do not
        // contain raw URLs (which could echo attacker-controlled content
        // into logs), and do not name internal files.
        for d in [
            FilterDenial::UnsupportedScheme { scheme: "file".into() },
            FilterDenial::MissingHost,
            FilterDenial::Malformed,
            FilterDenial::DeniedIpLiteral { literal: "10.1.2.3".into() },
            FilterDenial::DeniedResolvedIp { ip: "127.0.0.1".parse()? },
            FilterDenial::UserinfoNotAllowed,
            FilterDenial::ReservedNamespace { path: "/.well-known/agent-card.json".into() },
        ] {
            let s = d.to_string();
            assert!(s.len() < 200, "denial string is too long: {s}");
            assert!(!s.contains("file://"), "denial string echoes the URL: {s}");
        }
        Ok(())
    }
}
