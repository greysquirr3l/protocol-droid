//! The default SSRF denylist for A2A push-notification URLs.
//!
//! # What this is
//!
//! The production implementation of [`UrlFilter`].
//! It refuses every URL the OWASP API Security Top 10 (2023) API7
//! guidance names as a known SSRF primitive, in the order the spec names
//! them. A private-network deployment that needs to deliver to
//! `10.0.0.0/8` writes its own impl and passes it in; the production
//! `SsrfDenylist` is the safe default, not the only option.
//!
//! # The ranges, in one place
//!
//! | Family | Range | Why denied |
//! | --- | --- | --- |
//! | IPv4 loopback | `127.0.0.0/8` | The local machine. |
//! | IPv4 link-local | `169.254.0.0/16` | Includes the cloud metadata endpoint at `169.254.169.254`. |
//! | IPv4 private | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | RFC 1918. |
//! | IPv4 unspecified | `0.0.0.0/8` | The "any" address; resolving here dials the local machine. |
//! | IPv4 broadcast | `255.255.255.255/32` | Limited broadcast. |
//! | IPv4 documentation | `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24` | RFC 5737. |
//! | IPv4 this network | `0.0.0.0/8` | "This network". |
//! | IPv6 loopback | `::1/128` | The local machine, IPv6. |
//! | IPv6 unspecified | `::/128` | The "any" address, IPv6. |
//! | IPv6 link-local | `fe80::/10` | RFC 4291. |
//! | IPv6 ULA | `fc00::/7` | RFC 4193 — the IPv6 RFC 1918. |
//! | IPv6 documentation | `2001:db8::/32` | RFC 3849. |
//! | IPv4-mapped IPv6 | `::ffff:0:0/96` | The IPv4 range, wrapped — checked after the IPv4 check. |
//!
//! # Schemes
//!
//! `http` and `https` are the only accepted schemes. `file://`, `gopher://`,
//! `dict://`, `ftp://`, `ldap://`, and the rest of the URL-scheme zoo are
//! either impossible (no socket) or obvious SSRF primitives. The
//! `javascript:` and `data:` schemes are not URLs the `url` crate parses
//! as URLs anyway.
//!
//! # What the denylist deliberately does *not* do
//!
//! - It does not resolve DNS. A hostname that resolves to a denied IP at
//!   delivery time is still rejected — by `check_at_delivery`, on the
//!   resolved IP, not here. The split is the DNS-rebinding defense.
//! - It does not follow redirects. An HTTP 302 to `127.0.0.1` is the
//!   delivery adapter's problem to refuse; the denylist would have to
//!   pre-resolve to catch it, and pre-resolving is what DNS-rebinding
//!   exploits.
//! - It does not parse the path. A URL whose path is
//!   `/..%2F..%2Fetc%2Fpasswd` is an *application*-level problem on the
//!   remote end, not an SSRF one — the server POSTs the body, the
//!   receiver decides what to do with it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

use crate::application::a2a::{FilterDenial, UrlFilter};
use crate::domain::a2a::A2aVersion;

/// The default SSRF denylist.
///
/// Constructed once and shared; the filter is stateless and `Sync`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SsrfDenylist;

impl SsrfDenylist {
    /// The shared filter.
    pub const FILTER: Self = Self;

    /// Tests whether an IP is in a denied range.
    ///
    /// Public so a deployment-specific override can reuse the table — the
    /// ranges are the OWASP-cited set, not a policy choice.
    #[must_use]
    pub fn is_denied_ip(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => Self::is_denied_v4(v4),
            IpAddr::V6(v6) => Self::is_denied_v6(v6),
        }
    }

    fn is_denied_v4(v4: Ipv4Addr) -> bool {
        let [a, b, c, _d] = v4.octets();

        // 0.0.0.0/8 — "this network", the unspecified address, etc.
        a == 0
            // 127.0.0.0/8 — loopback.
            || a == 127
            // 10.0.0.0/8 — RFC 1918 private.
            || a == 10
            // 172.16.0.0/12 — RFC 1918 private.
            || (a == 172 && (16..=31).contains(&b))
            // 192.168.0.0/16 — RFC 1918 private.
            || (a == 192 && b == 168)
            // 169.254.0.0/16 — link-local, including 169.254.169.254 (cloud metadata).
            || (a == 169 && b == 254)
            // 192.0.2.0/24 — TEST-NET-1 (RFC 5737).
            || (a == 192 && b == 0 && c == 2)
            // 198.51.100.0/24 — TEST-NET-2 (RFC 5737).
            || (a == 198 && b == 51 && c == 100)
            // 203.0.113.0/24 — TEST-NET-3 (RFC 5737).
            || (a == 203 && b == 0 && c == 113)
            // 255.255.255.255/32 — limited broadcast.
            || v4 == Ipv4Addr::BROADCAST
            // 100.64.0.0/10 — RFC 6598 shared address space (carrier-grade NAT).
            || (a == 100 && (64..=127).contains(&b))
    }

    fn is_denied_v6(v6: Ipv6Addr) -> bool {
        // ::/128 — unspecified.
        v6 == Ipv6Addr::UNSPECIFIED
            // ::1/128 — loopback.
            || v6 == Ipv6Addr::LOCALHOST
            // fe80::/10 — link-local.
            || (v6.segments()[0] & 0xffc0) == 0xfe80
            // fc00::/7 — unique local address (RFC 4193, the IPv6 RFC 1918).
            || (v6.segments()[0] & 0xfe00) == 0xfc00
            // 2001:db8::/32 — documentation (RFC 3849).
            || v6.segments()[0] == 0x2001 && v6.segments()[1] == 0x0db8
    }

    /// The static check, factored out so `check_at_config` and
    /// `check_at_delivery` (for the URL portion) share the same logic.
    fn check_url(url: &Url, version: A2aVersion) -> Result<(), FilterDenial> {
        // Schemes first — file:// and gopher:// are SSRF primitives
        // independent of what the host resolves to.
        match url.scheme() {
            "http" | "https" => {}
            other => {
                return Err(FilterDenial::UnsupportedScheme {
                    scheme: other.to_owned(),
                });
            }
        }

        // Userinfo is a known SSRF primitive in URL parsers that
        // disagree about what `http://user:pass@host/path` means. The
        // server is not honouring any credential the URL claims to carry.
        if !url.username().is_empty() || url.password().is_some() {
            return Err(FilterDenial::UserinfoNotAllowed);
        }

        let host = url.host().ok_or(FilterDenial::MissingHost)?;
        match host {
            Host::Domain(_) => {
                // Hostname is deferred to the delivery-time check; the
                // static check cannot evaluate a name it has not
                // resolved. See `check_at_delivery`.
            }
            Host::Ipv4(v4) => {
                if Self::is_denied_v4(v4) {
                    return Err(FilterDenial::DeniedIpLiteral {
                        literal: v4.to_string(),
                    });
                }
            }
            Host::Ipv6(v6) => {
                if Self::is_denied_v6(v6) {
                    return Err(FilterDenial::DeniedIpLiteral {
                        literal: format!("[{v6}]"),
                    });
                }
            }
        }

        // The path the URL names. A push-notification URL aimed at the
        // server's own well-known path is a loopback primitive; refuse
        // it here so the receiver is not its own webhook.
        let path = url.path();
        if path.starts_with("/.well-known/") {
            return Err(FilterDenial::ReservedNamespace {
                path: path.to_owned(),
            });
        }

        // The version's reserved path is the same idea, version-tagged.
        // A2A v1.0 uses `/.well-known/agent-card.json`; the path check
        // above already catches it, but a future revision that picks a
        // different namespace would not, and `version` is here so this
        // branch can grow without changing the public signature.
        let _ = version;

        Ok(())
    }
}

impl UrlFilter for SsrfDenylist {
    fn check_at_config(&self, url: &Url, version: A2aVersion) -> Result<(), FilterDenial> {
        Self::check_url(url, version)
    }

    fn check_at_delivery(
        &self,
        url: &Url,
        resolved: IpAddr,
        version: A2aVersion,
    ) -> Result<(), FilterDenial> {
        // The URL itself is re-checked, in case a deployment changed
        // its policy between config and delivery, or the URL was
        // tampered with in the store.
        Self::check_url(url, version)?;

        // And the resolved IP — this is the DNS-rebinding defense.
        if Self::is_denied_ip(resolved) {
            return Err(FilterDenial::DeniedResolvedIp { ip: resolved });
        }

        // An IPv4-mapped IPv6 address (::ffff:127.0.0.1) is the resolved
        // IP for a hostname that returns a denied IPv4 over an IPv6
        // socket. Catch the mapping explicitly so the message names the
        // IPv4 the caller recognises, not the IPv6 wrapper.
        if let IpAddr::V6(v6) = resolved
            && let Some(v4) = v6.to_ipv4_mapped()
            && Self::is_denied_v4(v4)
        {
            return Err(FilterDenial::DeniedResolvedIp { ip: IpAddr::V4(v4) });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        clippy::panic,
        reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
    )]

    use std::net::IpAddr;
    use std::str::FromStr;

    use url::Url;

    use super::SsrfDenylist;
    use crate::application::a2a::{FilterDenial, UrlFilter};
    use crate::domain::a2a::A2aVersion;

    const V1_0: A2aVersion = A2aVersion::V1_0;

    fn url(s: &str) -> Url {
        Url::from_str(s).expect("test fixture must be a valid URL")
    }

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).expect("test fixture must be a valid IP")
    }

    // --- `is_denied_v4` is the table the OWASP guidance names ----------

    #[test]
    fn is_denied_v4_covers_every_owasp_cited_range() {
        // The cases the T24 audit called out by name.
        let denied = [
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "192.168.255.255",
            "169.254.0.1",
            "169.254.169.254", // cloud metadata endpoint
            "0.0.0.0",
            "255.255.255.255",
        ];
        for s in denied {
            let addr = ip(s);
            let IpAddr::V4(v4) = addr else {
                panic!("{s} is not IPv4");
            };
            assert!(
                SsrfDenylist::is_denied_v4(v4),
                "{s} should be denied per the OWASP API7 list"
            );
        }
    }

    #[test]
    fn is_denied_v4_allows_public_ranges() {
        let allowed = [
            "1.1.1.1",       // Cloudflare DNS
            "8.8.8.8",       // Google DNS
            "9.9.9.9",       // Quad9
            "93.184.216.34", // example.com
            "172.32.0.1",    // first address outside 172.16.0.0/12
            "172.15.0.1",    // last address outside
            "169.255.0.1",   // first address outside 169.254.0.0/16
            "169.253.0.1",   // last address outside
            "11.0.0.1",      // just outside 10.0.0.0/8
        ];
        for s in allowed {
            let IpAddr::V4(v4) = ip(s) else {
                panic!("{s} is not IPv4");
            };
            assert!(
                !SsrfDenylist::is_denied_v4(v4),
                "{s} should be allowed (public address)"
            );
        }
    }

    #[test]
    fn is_denied_v6_covers_loopback_and_ula() {
        let denied = ["::1", "::", "fe80::1", "fc00::1", "fd00::1", "2001:db8::1"];
        for s in denied {
            let IpAddr::V6(v6) = ip(s) else {
                panic!("{s} is not IPv6");
            };
            assert!(
                SsrfDenylist::is_denied_v6(v6),
                "{s} should be denied per the IPv6 denylist"
            );
        }
    }

    #[test]
    fn is_denied_v6_allows_public_ranges() {
        let allowed = [
            "2001:4860:4860::8888", // Google DNS
            "2606:4700:4700::1111", // Cloudflare DNS
        ];
        for s in allowed {
            let IpAddr::V6(v6) = ip(s) else {
                panic!("{s} is not IPv6");
            };
            assert!(
                !SsrfDenylist::is_denied_v6(v6),
                "{s} should be allowed (public IPv6)"
            );
        }
    }

    // --- `check_at_config` accepts public URLs and refuses denylist ---

    #[test]
    fn check_at_config_accepts_a_public_https_url() {
        SsrfDenylist::FILTER
            .check_at_config(&url("https://example.com/hook"), V1_0)
            .expect("the call must succeed");
    }

    #[test]
    fn check_at_config_refuses_loopback() {
        let err = SsrfDenylist::FILTER
            .check_at_config(&url("http://127.0.0.1/hook"), V1_0)
            .expect_err("loopback must be denied");
        assert_matches::assert_matches!(err, FilterDenial::DeniedIpLiteral { .. });
    }

    #[test]
    fn check_at_config_refuses_cloud_metadata() {
        let err = SsrfDenylist::FILTER
            .check_at_config(&url("http://169.254.169.254/latest/meta-data/"), V1_0)
            .expect_err("cloud metadata endpoint must be denied");
        assert_matches::assert_matches!(err, FilterDenial::DeniedIpLiteral { .. });
    }

    #[test]
    fn check_at_config_refuses_rfc_1918() {
        for s in [
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://192.168.1.1/",
        ] {
            let err = SsrfDenylist::FILTER
                .check_at_config(&url(s), V1_0)
                .expect_err("RFC 1918 must be denied");
            assert_matches::assert_matches!(err, FilterDenial::DeniedIpLiteral { .. });
        }
    }

    #[test]
    fn check_at_config_refuses_ipv6_loopback() {
        let err = SsrfDenylist::FILTER
            .check_at_config(&url("http://[::1]/hook"), V1_0)
            .expect_err("IPv6 loopback must be denied");
        assert_matches::assert_matches!(err, FilterDenial::DeniedIpLiteral { .. });
    }

    #[test]
    fn check_at_config_refuses_non_http_schemes() {
        for scheme in ["file", "gopher", "ftp", "ldap", "dict"] {
            let u = url(&format!("{scheme}://example.com/x"));
            let err = SsrfDenylist::FILTER
                .check_at_config(&u, V1_0)
                .expect_err(&format!("{scheme} must be denied"));
            let matched = matches!(err, FilterDenial::UnsupportedScheme { .. });
            assert!(matched, "{scheme} denial was {err:?}");
        }
    }

    #[test]
    fn check_at_config_refuses_userinfo() {
        let err = SsrfDenylist::FILTER
            .check_at_config(&url("https://attacker:pwn@example.com/"), V1_0)
            .expect_err("userinfo must be denied");
        assert_eq!(err, FilterDenial::UserinfoNotAllowed);
    }

    #[test]
    fn check_at_config_refuses_well_known_path() {
        // A push notification aimed at the agent's own well-known
        // path is a loopback primitive; the static check refuses it.
        let err = SsrfDenylist::FILTER
            .check_at_config(
                &url("https://example.com/.well-known/agent-card.json"),
                V1_0,
            )
            .expect_err("well-known path must be denied");
        assert_matches::assert_matches!(err, FilterDenial::ReservedNamespace { .. });
    }

    // --- `check_at_delivery` is the DNS-rebinding defense -------------

    #[test]
    fn check_at_delivery_refuses_a_dns_rebind_to_loopback() {
        // The URL string was acceptable at config time (hostname, no
        // literal). At delivery time the resolver returned 127.0.0.1.
        let err = SsrfDenylist::FILTER
            .check_at_delivery(
                &url("https://benign.example.com/hook"),
                ip("127.0.0.1"),
                V1_0,
            )
            .expect_err("DNS-rebind to loopback must be denied at delivery");
        assert_eq!(
            err,
            FilterDenial::DeniedResolvedIp {
                ip: ip("127.0.0.1"),
            }
        );
    }

    #[test]
    fn check_at_delivery_refuses_a_dns_rebind_to_metadata() {
        let err = SsrfDenylist::FILTER
            .check_at_delivery(
                &url("https://benign.example.com/hook"),
                ip("169.254.169.254"),
                V1_0,
            )
            .expect_err("DNS-rebind to cloud metadata must be denied at delivery");
        assert_eq!(
            err,
            FilterDenial::DeniedResolvedIp {
                ip: ip("169.254.169.254"),
            }
        );
    }

    #[test]
    fn check_at_delivery_unwraps_ipv4_mapped_ipv6() {
        // Some resolvers return 127.0.0.1 over an IPv6 socket as
        // ::ffff:127.0.0.1. The denylist should unwrap the mapping and
        // name the IPv4 the operator recognises, not the wrapper.
        let err = SsrfDenylist::FILTER
            .check_at_delivery(
                &url("https://benign.example.com/hook"),
                ip("::ffff:127.0.0.1"),
                V1_0,
            )
            .expect_err("IPv4-mapped loopback must be denied at delivery");
        assert_eq!(
            err,
            FilterDenial::DeniedResolvedIp {
                ip: ip("127.0.0.1"),
            },
            "the denial should name the IPv4 the operator recognises, not the wrapper"
        );
    }

    #[test]
    fn check_at_delivery_accepts_a_public_resolved_ip() {
        SsrfDenylist::FILTER
            .check_at_delivery(
                &url("https://benign.example.com/hook"),
                ip("93.184.216.34"),
                V1_0,
            )
            .expect("the call must succeed");
    }

    #[test]
    fn check_at_delivery_rechecks_the_url() {
        // If the stored URL has been tampered with between config and
        // delivery (a write-side bug, a misbehaving registry, an
        // attacker who pwned the persistence layer), the delivery
        // check should still catch the IP literal.
        let err = SsrfDenylist::FILTER
            .check_at_delivery(&url("http://10.0.0.1/hook"), ip("93.184.216.34"), V1_0)
            .expect_err("a tampered URL with a denied IP literal must still be denied");
        assert_matches::assert_matches!(err, FilterDenial::DeniedIpLiteral { .. });
    }
}
