//! The `Origin` check, which the specification makes a `MUST`.
//!
//! # What the specification says
//!
//! > When implementing HTTP with SSE transport:
//! >
//! > 1. Servers **MUST** validate the `Origin` header on all incoming
//! >    connections to prevent DNS rebinding attacks
//! > 2. When running locally, servers **SHOULD** bind only to localhost
//! >    (127.0.0.1) rather than all network interfaces (0.0.0.0)
//! > 3. Servers **SHOULD** implement proper authentication for all connections
//!
//! Cited: <https://modelcontextprotocol.io/specification/2024-11-05/basic/transports>
//!
//! This module is (1). (2) is a bind address and (3) is [`AuthPolicy`] in the
//! sibling `auth` module. Of the three, (1) is the only `MUST`, and it is the
//! one that is easiest to implement wrongly in a way that looks correct.
//!
//! # The attack
//!
//! DNS rebinding, in the shape it takes against a local MCP server:
//!
//! 1. The user visits `evil.example`, which serves a page.
//! 2. The page's DNS record has a short TTL and points at `127.0.0.1`.
//!    The user sees `http://127.0.0.1:8080/` in their address bar, and it is
//!    the real local server, so nothing looks wrong.
//! 3. The page now runs `fetch("http://127.0.0.1:8080/mcp", …)`. The browser
//!    treats this as **cross-origin** and attaches
//!    `Origin: http://evil.example`.
//! 4. The local server answers, because it authenticated nobody and the URL
//!    is correct, and a page in a browser tab can now enumerate tools, call
//!    them, and read the results.
//!
//! Step 4 is the one the server is supposed to prevent. The whole defence is
//! step 3's header: a browser **always** attaches `Origin` to a cross-origin
//! request, so "the `Origin` is not mine" is a signal that is present exactly
//! when the request is the one the `MUST` is about.
//!
//! # Why a *missing* `Origin` is allowed
//!
//! Because every non-browser client omits it, and refusing those would break
//! the transport for everyone except the attacker. A CLI, a Rust SDK, a
//! server-to-server call, and a `curl` all send no `Origin` — and those are
//! the legitimate majority. Failing closed on absent would not be more secure;
//! it would be secure against browsers and unusable for everything else, which
//! is the trade that produces a transport nobody runs.
//!
//! So the rule is deliberately asymmetric, and the asymmetry is the design:
//!
//! - **Present and not ours** → [`Refusal::ForeignOrigin`]. The browser case,
//!   and the only one that can carry a forged cross-site request.
//! - **Present and ours** → allowed. A same-origin page, or a browser client
//!   whose origin the operator listed.
//! - **Absent** → allowed. Not a browser, so not the rebinding case.
//!
//! # Why a *present* `Origin` is not automatically an attack
//!
//! Because the ordinary local development case is a page on
//! `http://localhost:3000` talking to a server on `http://127.0.0.1:8080`.
//! That `Origin` is genuinely not the server's own, it is genuinely
//! cross-origin, and it is legitimate. A policy that refused it would make
//! every front-end developer conclude the `MUST` was unusable and disable it —
//! so [`OriginPolicy::allow`] takes an explicit list, and refusing everything
//! by default is the *safe* direction to fail in when the operator has not
//! configured it.
//!
//! The default is therefore: no listed origins, and the server's own origin
//! as configured. A same-origin browser page works; every other browser page
//! is refused; every non-browser client works.

use core::str::FromStr;

use crate::adapters::primary::transport::http::Refusal;

/// The scheme-and-authority an `Origin` names, compared as such.
///
/// Lowercased on parse, because RFC 6454 §6.2.2 makes the comparison
/// case-insensitive for scheme and host: a browser sends `HTTP://Example.COM`
/// and a byte-comparing server refuses a page it just served. A path or query
/// in an `Origin` is refused outright — the header is scheme-and-host-only,
/// and a value carrying more is not a value this policy can reason about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin(String);

impl Origin {
    /// The origin as sent, lowercased in its scheme and host.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The host this origin names, without the scheme and without the port.
    ///
    /// Brackets are stripped, so an IPv6 host reads as `::1` rather than as
    /// `[::1]`, which is the form every comparison in this module wants.
    #[must_use]
    pub fn host(&self) -> &str {
        let authority = self
            .0
            .split_once("://")
            .map_or(self.0.as_str(), |(_, rest)| rest);
        host_of(authority)
    }

    /// Whether this origin names the loopback interface.
    ///
    /// Exactly `localhost`, `127.0.0.1` or `::1`, and nothing else. This is
    /// the check that keeps a rebinding defence from rebinding: `localhost`
    /// is compared whole, so `localhost.evil.example` and `127.0.0.1.evil.example`
    /// are not loopback and are not excused by this.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        is_loopback_host(self.host())
    }
}

/// The host from an authority, which is `host[:port]` or `[host]:port`.
///
/// The bracketed form is handled before the port is split off, because the
/// port separator inside `[::1]` is the `:` in the address itself: splitting
/// `[::1]:18099` on the first colon yields `[` and `1]:18099`, which names
/// nothing.
fn host_of(authority: &str) -> &str {
    if let Some(bracketed) = authority.strip_prefix('[') {
        return bracketed.split(']').next().unwrap_or(authority);
    }
    match authority.split_once(':') {
        // Only a numeric suffix is a port. `localhost:8080.evil.example`
        // carries a colon and still names a single host, so splitting there
        // would read the host as `localhost` and excuse it as loopback —
        // which is the one shape that turns this function into the hole it
        // exists to close.
        Some((host, port))
            if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            host
        }
        _ => authority,
    }
}

/// Whether `host` is one of the three names the specification's own suite
/// treats as localhost.
fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// Why a string is not a usable `Origin`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OriginError {
    /// The value was empty.
    #[error("an Origin header was present but empty")]
    Empty,

    /// The value was not `scheme://host[:port]`.
    #[error("an Origin header must be scheme://host[:port] with no path")]
    NotAnOrigin,

    /// The scheme was not `http` or `https`.
    ///
    /// A browser's `Origin` for a page it loaded over HTTP or HTTPS is `http`
    /// or `https` and nothing else. Anything else came from something that is
    /// not a browser, which means it is not the case this check exists for, and
    /// it is refused rather than passed through as a string comparison.
    #[error("an Origin header must use the http or https scheme")]
    UnsupportedScheme,
}

impl FromStr for Origin {
    type Err = OriginError;

    /// # Errors
    ///
    /// [`OriginError::Empty`] for an empty value,
    /// [`OriginError::UnsupportedScheme`] for a scheme other than HTTP(S), and
    /// [`OriginError::NotAnOrigin`] for anything that is not
    /// `scheme://host[:port]` — including a bare host, a trailing path, or a
    /// `null` origin, which is what a sandboxed frame and some redirects send.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(OriginError::Empty);
        }
        let Some((scheme, rest)) = value.split_once("://") else {
            return Err(OriginError::NotAnOrigin);
        };
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(OriginError::UnsupportedScheme);
        }
        // The rest must be host or host:port and nothing else. A `/` in here
        // is a path, which RFC 6454 does not put in an `Origin`; accepting and
        // ignoring it would mean two different headers compare equal as
        // strings while meaning different things to the browser that sent them.
        if rest.is_empty() || rest.contains('/') {
            return Err(OriginError::NotAnOrigin);
        }
        Ok(Self(format!("{scheme}://{}", rest.to_ascii_lowercase())))
    }
}

/// Which `Origin`s this server answers.
///
/// Not a trait and not a port: the check is a pure function over two strings,
/// and a port trait here would be a dynamic dispatch bought for nothing. The
/// type exists so the *default* can be fail-secure and the comparison can be
/// spelled once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginPolicy {
    /// Origins this server answers for, beyond its own.
    allowed: Vec<Origin>,
    /// The origin this server considers to be itself.
    own: Option<Origin>,
}

impl OriginPolicy {
    /// A policy that answers only its own origin.
    ///
    /// `own` is `None` when the server was not told its own origin — behind a
    /// proxy that rewrites `Host`, or bound to `0.0.0.0` and reached by
    /// several names. That is **not** a reason to answer everything: with no
    /// known own-origin the policy refuses every `Origin` that is present,
    /// which is the fail-secure direction. A server in that position has to
    /// say what it serves, because nothing else can work it out.
    #[must_use]
    pub const fn strict(own: Option<Origin>) -> Self {
        Self {
            allowed: Vec::new(),
            own,
        }
    }

    /// Adds an origin this server answers for.
    ///
    /// The list is exactly what the operator wrote: no wildcard, no suffix
    /// match, no parent domain. A suffix rule (`evil-example.com` matching
    /// `not-evil-example.com`) is the sort of thing that gets added once,
    /// for a legitimate case, and never looked at again.
    #[must_use]
    pub fn allowing(mut self, origin: Origin) -> Self {
        self.allowed.push(origin);
        self
    }

    /// The origin this server considers its own, if it was told one.
    #[must_use]
    pub const fn own(&self) -> Option<&Origin> {
        self.own.as_ref()
    }

    /// The origins answered for, including this server's own.
    pub fn permitted(&self) -> impl Iterator<Item = &Origin> {
        self.own.iter().chain(self.allowed.iter())
    }

    /// Decides whether a request carrying `header` is answered.
    ///
    /// `None` means the header was absent, which is a non-browser client and
    /// is allowed — see the module docs for why that asymmetry is the design
    /// and not an oversight.
    ///
    /// A loopback origin is answered whatever the allow-list says, and that is
    /// not a hole in it. `Origin` is set by the browser and names the page the
    /// request came *from*, so a page served from `http://localhost:8080`
    /// genuinely sends `http://localhost:8080` — and a rebound page at
    /// `evil.example` sends `http://evil.example`, which is not loopback and is
    /// refused here exactly as before. What a loopback allowlist would get
    /// wrong is a suffix match, so the host is compared whole.
    ///
    /// The comparison is exact and case-insensitive-in-scheme-and-host, done
    /// once at parse. A *substring* or *suffix* match is the tempting
    /// alternative and is the bug this type is shaped to prevent: an allowlist
    /// of `localhost` that also answers `localhost.evil.example` is a
    /// rebinding defence that rebinds.
    pub fn check(&self, header: Option<&str>) -> Result<(), Refusal> {
        let Some(raw) = header else {
            return Ok(());
        };
        let origin = Origin::from_str(raw).map_err(|_| Refusal::ForeignOrigin)?;
        if origin.is_loopback() || self.permitted().any(|permitted| permitted == &origin) {
            Ok(())
        } else {
            Err(Refusal::ForeignOrigin)
        }
    }

    /// Decides whether a request whose `Host` is `header` is answered.
    ///
    /// `Host` is the header a rebinding attack actually controls, and checking
    /// only `Origin` leaves the attack open to a client that omits it: a
    /// browser rebound onto `127.0.0.1` sends `Host: evil.example`, and a
    /// non-browser client can send whatever it likes. A browser cannot forge
    /// `Host` — it is set from the URL it dialled — so a `Host` that is not
    /// loopback and not one this server was told to serve names a request for
    /// somewhere else.
    ///
    /// Compared on the *host* rather than the whole authority, because the
    /// port is not part of the question: a client reaching this server on the
    /// port it was bound to names a host, and a loopback host on a different
    /// port is a different server that this policy has no opinion about.
    ///
    /// `None` means the header was absent. HTTP/1.1 requires it and HTTP/2
    /// carries it as `:authority`, so this is a proxy that stripped it rather
    /// than a client that had none; it is refused, because a check that
    /// answers "no evidence either way" cannot be the defence.
    pub fn check_host(&self, header: Option<&str>) -> Result<(), Refusal> {
        let Some(raw) = header else {
            return Err(Refusal::ForeignOrigin);
        };
        let host = host_of(raw.trim());
        if is_loopback_host(host) || self.permitted().any(|permitted| permitted.host() == host) {
            Ok(())
        } else {
            Err(Refusal::ForeignOrigin)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Origin, OriginError, OriginPolicy};
    use crate::adapters::primary::transport::http::Refusal;
    use core::str::FromStr;

    type TestError = Box<dyn std::error::Error>;

    /// Asserts `value` parses to an origin reading back as `expected`.
    fn accepts(value: &str, expected: &str) -> Result<(), TestError> {
        let origin = Origin::from_str(value)?;
        if origin.as_str() != expected {
            return Err(format!("{value:?} read back as {:?}", origin.as_str()).into());
        }
        Ok(())
    }

    /// Asserts `value` is refused, and with `expected`.
    fn refuses(value: &str, expected: &OriginError) -> Result<(), TestError> {
        match Origin::from_str(value) {
            Err(error) if &error == expected => Ok(()),
            Err(error) => {
                Err(format!("{value:?} refused as {error:?}, wanted {expected:?}").into())
            }
            Ok(origin) => Err(format!("{value:?} was accepted as {:?}", origin.as_str()).into()),
        }
    }

    /// A policy answering only `http://127.0.0.1:8080`.
    fn policy() -> Result<OriginPolicy, TestError> {
        Ok(OriginPolicy::strict(Some(Origin::from_str(
            "http://127.0.0.1:8080",
        )?)))
    }

    /// RFC 6454 §6.2.2: the comparison is case-insensitive in scheme and
    /// host, and a byte-comparing server refuses the page it just served.
    #[test]
    fn scheme_and_host_are_lowercased() -> Result<(), TestError> {
        accepts("HTTP://Example.COM", "http://example.com")
    }

    #[test]
    fn a_trailing_path_is_refused() -> Result<(), TestError> {
        refuses("http://example.com/steal", &OriginError::NotAnOrigin)
    }

    /// What a sandboxed frame and some redirects send. Not a browser talking
    /// to a local server, so there is nothing here to reason about.
    #[test]
    fn the_null_origin_is_refused() -> Result<(), TestError> {
        refuses("null", &OriginError::NotAnOrigin)
    }

    #[test]
    fn a_non_http_scheme_is_refused() -> Result<(), TestError> {
        refuses("file://example.com", &OriginError::UnsupportedScheme)
    }

    /// The attack, step 3: the header a browser attaches to the forged
    /// cross-site request. Refused, because it is not the server's own.
    #[test]
    fn a_foreign_origin_is_refused() -> Result<(), TestError> {
        if policy()?.check(Some("http://evil.example")).is_ok() {
            return Err("a foreign origin was answered".into());
        }
        Ok(())
    }

    /// Step 3 in the other direction: the legitimate local dev page.
    ///
    /// Listed explicitly, because refusing it is what makes an operator turn
    /// the check off entirely, and a disabled check is worse than a wrong
    /// default.
    #[test]
    fn a_listed_origin_is_answered() -> Result<(), TestError> {
        let policy = policy()?.allowing(Origin::from_str("http://localhost:3000")?);
        if policy.check(Some("http://localhost:3000")).is_err() {
            return Err("a listed origin was refused".into());
        }
        Ok(())
    }

    /// The suffix bug, stated as a test.
    ///
    /// An allowlist matching by suffix answers `not-localhost` for a policy
    /// that lists `localhost`, and answers `localhost.evil.example` for one
    /// that lists `localhost.evil.example`'s parent. The comparison is exact
    /// so neither can happen.
    ///
    /// Stated on `app.internal` rather than on `localhost`, because loopback
    /// hosts are now answered whatever the allow-list says (see
    /// [`OriginPolicy::check`]) and a loopback case would pass for the wrong
    /// reason. What this is about is the allow-list, so the allow-list's host
    /// is the one under test.
    #[test]
    fn a_neighbouring_host_is_not_answered() -> Result<(), TestError> {
        let policy = policy()?.allowing(Origin::from_str("http://app.internal:3000")?);
        for neighbour in [
            "http://notapp.internal:3000",
            "http://app.internal.evil.example:3000",
            "http://app.internal:3001",
            "http://app.internal:3000.evil.example",
            "http://app.internalx:3000",
        ] {
            if policy.check(Some(neighbour)).is_ok() {
                return Err(format!("{neighbour} was answered").into());
            }
        }
        Ok(())
    }

    /// **Every** listed origin answers, not just the first.
    ///
    /// This test exists because the allowlist was originally exercised with a
    /// single entry, and a `first()`-based check passed all of them: with one
    /// permitted origin, "the first one matches" and "any one matches" are the
    /// same predicate. A user with a dev server on `:3000` and a second
    /// client on `:3001` is the ordinary case for listing more than one, and a
    /// `first()` check silently refuses the second — which reads as "the
    /// allowlist does not work" and gets diagnosed by deleting the allowlist.
    #[test]
    fn every_listed_origin_answers_not_only_the_first() -> Result<(), TestError> {
        let policy = policy()?
            .allowing(Origin::from_str("http://localhost:3000")?)
            .allowing(Origin::from_str("http://localhost:3001")?)
            .allowing(Origin::from_str("http://app.internal")?);

        for listed in [
            "http://localhost:3000",
            "http://localhost:3001",
            "http://app.internal",
            // And the server's own, which is answered alongside them.
            "http://127.0.0.1:8080",
        ] {
            if policy.check(Some(listed)).is_err() {
                return Err(format!("{listed} was refused").into());
            }
        }
        Ok(())
    }

    /// The port is part of the origin, so a different port is a different
    /// origin and is refused.
    ///
    /// On `app.internal`, because loopback is answered on any port — the one
    /// place this module deliberately answers something the allow-list did
    /// not name, and it is not a rebinding hole because `Origin` names the
    /// page rather than the address, so a rebound page cannot present a
    /// loopback one. The allow-list stays exact.
    #[test]
    fn a_different_port_is_a_different_origin() -> Result<(), TestError> {
        if policy()?.check(Some("http://app.internal:9999")).is_ok() {
            return Err("a different port was answered".into());
        }
        Ok(())
    }

    /// Loopback is answered on any port, which is the loosening this module
    /// made on purpose.
    ///
    /// Pinned rather than left to the tests above, so that a reader who finds
    /// it surprising has the reason here rather than in a diff: a browser
    /// reaching a server on `localhost:18099` sends `Origin:
    /// http://localhost:18099`, and refusing that refuses every browser client
    /// and the conformance suite with it.
    #[test]
    fn a_loopback_origin_is_answered_on_any_port() -> Result<(), TestError> {
        let policy = policy()?;
        for origin in [
            "http://localhost:1",
            "http://localhost:9999",
            "http://127.0.0.1:65535",
            "http://[::1]:18099",
        ] {
            if policy.check(Some(origin)).is_err() {
                return Err(format!("{origin} was refused").into());
            }
        }
        Ok(())
    }

    /// The asymmetry, and the whole reason the transport works for anyone who
    /// is not a browser.
    #[test]
    fn an_absent_origin_is_answered() -> Result<(), TestError> {
        if policy()?.check(None).is_err() {
            return Err("a client sending no Origin was refused".into());
        }
        Ok(())
    }

    /// A server that does not know its own origin refuses every `Origin` that
    /// is present, rather than answering all of them.
    #[test]
    fn a_server_that_knows_its_own_origin_still_refuses_foreign_ones() -> Result<(), TestError> {
        let policy = OriginPolicy::strict(None);
        if policy.check(Some("http://anything.example")).is_ok() {
            return Err("a server with no known origin answered one".into());
        }
        Ok(())
    }

    /// A present-but-malformed header is refused, not treated as absent. The
    /// difference is the attack: a browser sends a well-formed `Origin`, so a
    /// malformed one did not come from the browser case the `MUST` names.
    #[test]
    fn a_malformed_origin_is_refused_rather_than_ignored() -> Result<(), TestError> {
        if policy()?.check(Some("garbage")).is_ok() {
            return Err("a malformed Origin was answered".into());
        }
        Ok(())
    }

    /// The refusal carries nothing the attacker chose. Echoing a forged
    /// `Origin` back in a response body reflects untrusted input to its sender.
    #[test]
    fn the_refusal_does_not_echo_the_header() -> Result<(), TestError> {
        let forged = "http://evil.example/steal?token=abc";
        let refusal = policy()?.check(Some(forged)).err();
        let Some(refusal) = refusal else {
            return Err("a foreign origin was answered".into());
        };
        if refusal != Refusal::ForeignOrigin {
            return Err(format!("refused as {refusal:?}").into());
        }
        let body = refusal.body().to_string();
        if body.contains("evil.example") || body.contains("token") {
            return Err(format!("the refusal echoed the header: {body}").into());
        }
        Ok(())
    }

    /// The `MUST` wants a status a browser and a proxy both understand, and no
    /// challenge — a `WWW-Authenticate` would be advice to authenticate with
    /// something that cannot help.
    #[test]
    fn the_refusal_is_a_plain_403() -> Result<(), TestError> {
        if Refusal::ForeignOrigin.status() != axum::http::StatusCode::FORBIDDEN {
            return Err("the refusal is not a 403".into());
        }
        Ok(())
    }

    /// The three loopback spellings, each with and without a port.
    ///
    /// These are the values the specification's own conformance suite calls
    /// valid localhost, and a server that refuses them refuses every browser
    /// client and the suite with it.
    #[test]
    fn a_loopback_origin_is_answered() -> Result<(), TestError> {
        let policy = policy()?;
        for host in [
            "localhost:8080",
            "127.0.0.1:8080",
            "[::1]:8080",
            "localhost",
        ] {
            let origin = format!("http://{host}");
            if let Err(refusal) = policy.check(Some(&origin)) {
                return Err(format!("{origin} was refused as {refusal:?}").into());
            }
        }
        Ok(())
    }

    /// Excusing loopback must not become excusing anything that looks like it.
    ///
    /// The whole defence rests on this comparison being whole rather than a
    /// suffix: `localhost.evil.example` and `127.0.0.1.evil.example` are
    /// attacker-controlled names that resolve to loopback, and an allowlist
    /// that answers them is a rebinding defence that rebinds.
    #[test]
    fn a_name_that_only_looks_like_loopback_is_refused() -> Result<(), TestError> {
        let policy = policy()?;
        for origin in [
            "http://localhost.evil.example",
            "http://127.0.0.1.evil.example",
            "http://notlocalhost",
            "http://127.0.0.2",
            "http://localhost:8080.evil.example",
        ] {
            let refused = policy.check(Some(origin)).err();
            if refused.is_none() {
                return Err(format!("{origin} was answered").into());
            }
        }
        Ok(())
    }

    /// `Host` is the header the attack controls, so it is checked too.
    ///
    /// `Host` is absent from no non-browser client that reaches a real
    /// server, and a rebinding request names the attacker's domain in it.
    #[test]
    fn a_loopback_host_is_answered_and_a_foreign_one_is_not() -> Result<(), TestError> {
        let policy = policy()?;
        for host in [
            "localhost:8080",
            "127.0.0.1:8080",
            "[::1]:8080",
            "localhost",
        ] {
            if let Err(refusal) = policy.check_host(Some(host)) {
                return Err(format!("Host {host} was refused as {refusal:?}").into());
            }
        }
        for host in [
            "evil.example",
            "evil.example:8080",
            "localhost.evil.example",
        ] {
            let refused = policy.check_host(Some(host)).err();
            if refused.is_none() {
                return Err(format!("Host {host} was answered").into());
            }
        }
        Ok(())
    }

    /// A stripped `Host` is refused rather than waved through.
    ///
    /// Absent is not "allowed" here even though it is for `Origin`: `Origin`
    /// is legitimately absent from a CLI, but `Host` is required on every
    /// HTTP/1.1 request, so its absence is evidence of a proxy rewriting the
    /// request and not of a client that had no name to give.
    #[test]
    fn an_absent_host_is_refused() -> Result<(), TestError> {
        if policy()?.check_host(None).is_ok() {
            return Err("a request with no Host was answered".into());
        }
        Ok(())
    }

    /// A configured origin answers its own host, whatever the port.
    ///
    /// An operator who names an origin they serve has answered the host
    /// question; requiring the port to match as well would refuse a request
    /// that arrived on a port the deployment redirects here.
    #[test]
    fn a_configured_origin_answers_its_own_host() -> Result<(), TestError> {
        let policy = OriginPolicy::strict(None).allowing(Origin::from_str("https://mcp.example")?);
        for host in ["mcp.example", "mcp.example:8443"] {
            if let Err(refusal) = policy.check_host(Some(host)) {
                return Err(format!("Host {host} was refused as {refusal:?}").into());
            }
        }
        if policy.check_host(Some("other.example")).is_ok() {
            return Err("an unconfigured host was answered".into());
        }
        Ok(())
    }
}
