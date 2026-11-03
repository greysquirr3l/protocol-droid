//! The endpoint rules, tested against the specification's own words.
//!
//! The set of loopback addresses comes from `docs/protocols/http.md`'s
//! "HTTPS Enforcement" section: `localhost`, `127.0.0.0/8`, `::1`, and their
//! wildcard and IPv4-mapped forms.

use crate::domain::utcp::endpoint::{
    Endpoint, EndpointError, check_declared, classify, is_loopback_host,
};

/// Why a test failed.
#[derive(Debug)]
enum Failure {
    /// A URL was refused that the test expected to be allowed.
    Refused(String),
    /// A URL was allowed that the test expected to be refused.
    Allowed(String),
    /// The endpoint rules refused something.
    Endpoint(EndpointError),
    /// The URL parsed but did not classify as the test expected.
    NotAsExpected {
        /// The URL under test.
        url: String,
        /// What it classified as.
        found: &'static str,
        /// What the test expected.
        expected: &'static str,
    },
}

impl From<EndpointError> for Failure {
    fn from(error: EndpointError) -> Self {
        Self::Endpoint(error)
    }
}

impl core::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused(url) => write!(formatter, "`{url}` was refused"),
            Self::Allowed(url) => write!(formatter, "`{url}` was allowed"),
            Self::Endpoint(error) => write!(formatter, "the endpoint rules refused it: {error}"),
            Self::NotAsExpected {
                url,
                found,
                expected,
            } => write!(formatter, "`{url}` classified as {found}, not {expected}"),
        }
    }
}

/// The hosts the specification names as loopback.
const LOOPBACK_HOSTS: [&str; 10] = [
    "localhost",
    "LOCALHOST",
    "127.0.0.1",
    "127.0.0.2",
    "127.1.2.3",
    "127.255.255.255",
    "::1",
    "[::1]",
    "::ffff:127.0.0.1",
    "0.0.0.0",
];

/// The hosts that are not loopback, however they are spelled.
const REMOTE_HOSTS: [&str; 8] = [
    "example.com",
    "127.0.0.1.example.com",
    "localhost.example.com",
    "127.0.0.1.evil.example",
    "128.0.0.1",
    "10.0.0.1",
    "::2",
    "notlocalhost",
];

#[test]
fn every_loopback_host_the_specification_names_is_loopback() {
    for host in LOOPBACK_HOSTS {
        assert!(
            is_loopback_host(host),
            "`{host}` is a loopback address by the specification's rule",
        );
    }
}

#[test]
fn a_name_that_merely_looks_loopback_is_not_loopback() {
    for host in REMOTE_HOSTS {
        assert!(
            !is_loopback_host(host),
            "`{host}` is not a literal loopback address",
        );
    }
}

#[test]
fn https_is_allowed_to_any_host() -> Result<(), Failure> {
    let url = "https://api.example.com/utcp";
    let endpoint = classify(url).map_err(|error| Failure::Refused(error.to_string()))?;
    if endpoint.is_loopback() {
        return Err(Failure::NotAsExpected {
            url: url.to_owned(),
            found: "loopback",
            expected: "secure",
        });
    }
    Ok(())
}

#[test]
fn plain_http_is_allowed_to_loopback() -> Result<(), Failure> {
    let url = "http://127.0.0.1:3000/utcp";
    let endpoint = classify(url).map_err(|error| Failure::Refused(error.to_string()))?;
    if !endpoint.is_loopback() {
        return Err(Failure::NotAsExpected {
            url: url.to_owned(),
            found: "secure",
            expected: "loopback",
        });
    }
    Ok(())
}

#[test]
fn plain_http_to_a_remote_host_is_refused() -> Result<(), Failure> {
    let outcome = classify("http://api.example.com/utcp");
    if !matches!(outcome, Err(EndpointError::NotSecure { .. })) {
        return Err(Failure::Allowed("http://api.example.com/utcp".to_owned()));
    }
    Ok(())
}

#[test]
fn a_scheme_that_is_not_http_is_refused() -> Result<(), Failure> {
    let outcome = classify("ftp://api.example.com/utcp");
    if !matches!(outcome, Err(EndpointError::UnsupportedScheme { .. })) {
        return Err(Failure::Allowed("ftp://api.example.com/utcp".to_owned()));
    }
    Ok(())
}

#[test]
fn a_url_that_is_not_a_url_is_refused() -> Result<(), Failure> {
    let outcome = classify("not a url at all");
    if !matches!(outcome, Err(EndpointError::Unparseable { .. })) {
        return Err(Failure::Allowed("not a url at all".to_owned()));
    }
    Ok(())
}

#[test]
fn a_remote_manual_may_not_declare_a_loopback_tool() -> Result<(), Failure> {
    let origin = match classify("https://api.example.com/utcp") {
        Ok(endpoint) => endpoint,
        Err(error) => return Err(Failure::Refused(error.to_string())),
    };
    let target = match classify("http://127.0.0.1:9000/tool") {
        Ok(endpoint) => endpoint,
        Err(error) => return Err(Failure::Refused(error.to_string())),
    };
    let outcome = check_declared(&origin, "https://api.example.com/utcp", &target);
    if !matches!(outcome, Err(EndpointError::RemoteDeclaresLoopback { .. })) {
        return Err(Failure::Allowed(
            "a loopback tool on a remote manual".to_owned(),
        ));
    }
    Ok(())
}

#[test]
fn a_local_manual_may_declare_a_loopback_tool() -> Result<(), Failure> {
    let origin = match classify("http://127.0.0.1:3000/utcp") {
        Ok(endpoint) => endpoint,
        Err(error) => return Err(Failure::Refused(error.to_string())),
    };
    let target = match classify("http://127.0.0.1:9000/tool") {
        Ok(endpoint) => endpoint,
        Err(error) => return Err(Failure::Refused(error.to_string())),
    };
    check_declared(&origin, "http://127.0.0.1:3000/utcp", &target)?;
    Ok(())
}

#[test]
fn a_remote_manual_may_declare_a_remote_tool() -> Result<(), Failure> {
    let origin = match classify("https://api.example.com/utcp") {
        Ok(endpoint) => endpoint,
        Err(error) => return Err(Failure::Refused(error.to_string())),
    };
    let target = match classify("https://tools.example.com/call") {
        Ok(endpoint) => endpoint,
        Err(error) => return Err(Failure::Refused(error.to_string())),
    };
    check_declared(&origin, "https://api.example.com/utcp", &target)?;
    Ok(())
}

#[test]
fn two_hosts_are_the_same_when_they_differ_only_in_case() {
    let left = Endpoint::Secure {
        host: "Example.COM".to_owned(),
    };
    let right = Endpoint::Secure {
        host: "example.com".to_owned(),
    };
    assert!(left.same_host(&right));
}

/// A remote host may not redirect into loopback.
///
/// The failure this prevents is a remote server steering the request at the
/// client's own services.
#[test]
fn a_remote_redirect_into_loopback_is_refused() -> Result<(), Failure> {
    let remote = classify("https://api.example.com/utcp")?;
    let loopback = classify("http://127.0.0.1:3000/utcp")?;
    assert!(remote.may_redirect_to(&loopback).is_err());
    Ok(())
}

/// A loopback discovery may redirect out to a remote host.
///
/// The content it reaches is remote, and the endpoint it lands on says so.
#[test]
fn a_loopback_redirect_out_to_a_remote_host_is_allowed() -> Result<(), Failure> {
    let loopback = classify("http://127.0.0.1:3000/utcp")?;
    let remote = classify("https://api.example.com/utcp")?;
    assert!(loopback.may_redirect_to(&remote).is_ok());
    Ok(())
}

/// A redirect within loopback is allowed: it stays inside the same trust
/// boundary it started in.
#[test]
fn a_loopback_redirect_within_loopback_is_allowed() -> Result<(), Failure> {
    let first = classify("http://127.0.0.1:3000/utcp")?;
    let second = classify("http://127.0.0.1:4000/utcp")?;
    assert!(first.may_redirect_to(&second).is_ok());
    Ok(())
}

/// A remote redirect to another remote host is allowed: the origin changed,
/// but the trust boundary did not.
#[test]
fn a_remote_redirect_to_another_remote_host_is_allowed() -> Result<(), Failure> {
    let first = classify("https://api.example.com/utcp")?;
    let second = classify("https://cdn.example.com/utcp")?;
    assert!(first.may_redirect_to(&second).is_ok());
    Ok(())
}
