//! Fetching a provider's manual.
//!
//! # Where a manual lives
//!
//! The specification says a provider publishes its manual at `GET /utcp` on
//! its own origin. So a caller gives this client the provider's base URL and
//! the client appends the path — unless the URL already carries it, because a
//! provider that publishes its manual somewhere else is not a provider this
//! build can refuse to read.
//!
//! # Why the media type is checked
//!
//! The specification says the manual is JSON. A peer that answers `200` with
//! an HTML error page is not a provider, and parsing it as a manual would
//! report "the document is malformed" when the truth is "the thing you
//! reached is not a manual at all". The two send the reader to different
//! places, so they are told apart.
//!
//! # What is checked before the request goes out
//!
//! Four things, each of which the specification or the transport rules
//! require, and each of which this client cannot do without:
//!
//! - **The URL is one this build will connect to.** HTTPS, or plain HTTP to
//!   a literal loopback address. See [`crate::domain::utcp::endpoint`].
//! - **Redirects are re-validated at every hop.** The client does not
//!   follow them automatically; it follows them itself, checking each hop,
//!   and refusing a hop that leaves the allowed set.
//! - **The request has a deadline.** A provider that accepts the connection
//!   and then says nothing must not hold the client forever.
//! - **The registration says what the manual's tools may be called over.**
//!   That is the [`crate::domain::utcp::policy::ProtocolPolicy`], and it
//!   comes from the caller, never from the document.
//!
//! # The registration template
//!
//! [`ManualClient::new`] builds an HTTP registration, because the manual is
//! read over HTTP. The specification's default then permits only `http`
//! tools. A provider whose manual names `mcp` or `cli` tools needs the
//! caller to say so explicitly, through
//! [`ManualClient::allowing_protocols`] — because a document that names a
//! `cli` tool is asking the client to run a program, and that ask should
//! not be answered by the document itself.

use std::time::Duration;

use crate::adapters::secondary::utcp::error::UtcpError;
use crate::domain::utcp::Manual;
use crate::domain::utcp::call_template::{CallTemplate, HttpMethod};
use crate::domain::utcp::endpoint::{self, Endpoint};

/// The path the specification puts a manual at.
const MANUAL_PATH: &str = "/utcp";

/// How long the client waits for the manual.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The most redirects this client will follow.
const MAX_REDIRECTS: usize = 5;

/// Reads manuals over HTTP.
#[derive(Debug, Clone)]
pub struct ManualClient {
    client: reqwest::Client,
    url: String,
    registration: CallTemplate,
    timeout: Duration,
}

impl ManualClient {
    /// A client that reads the manual at `base`.
    ///
    /// `base` is the provider's origin. The specification's `/utcp` path is
    /// appended to it unless it already ends with one.
    ///
    /// The registration this client builds permits only `http` tools, which
    /// is the specification's default for a manual read over HTTP. Use
    /// [`ManualClient::allowing_protocols`] when the provider's manual names
    /// tools over another protocol.
    ///
    /// # Errors
    ///
    /// [`UtcpError::Endpoint`] when `base` is not a URL this build will
    /// connect to, and [`UtcpError::Transport`] when the HTTP client cannot
    /// be built.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::adapters::secondary::utcp::ManualClient;
    ///
    /// let client = ManualClient::new("https://api.example.com")?;
    /// assert_eq!(client.url(), "https://api.example.com/utcp");
    /// # Ok::<(), protocol_droid::adapters::secondary::utcp::UtcpError>(())
    /// ```
    pub fn new(base: &str) -> Result<Self, UtcpError> {
        let trimmed = base.trim_end_matches('/');
        let url = if trimmed.ends_with(MANUAL_PATH) {
            trimmed.to_owned()
        } else {
            format!("{trimmed}{MANUAL_PATH}")
        };
        endpoint::classify(&url)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(|error| UtcpError::Transport {
                detail: error.to_string(),
            })?;
        let mut registration = CallTemplate::http("manual", &url, HttpMethod::Get);
        registration.allowed_communication_protocols = None;
        Ok(Self {
            client,
            url,
            registration,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// The URL this client reads.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The registration this client reads under.
    ///
    /// The template says how the manual was reached and which protocols its
    /// tools may use. It is the caller's side of the bargain, and the
    /// document does not get to change it.
    #[must_use]
    pub const fn registration(&self) -> &CallTemplate {
        &self.registration
    }

    /// Permits the manual's tools to be called over these protocols as well.
    ///
    /// The specification's `allowed_communication_protocols`. Without this,
    /// a manual read over HTTP may only have HTTP tools — the manual's own
    /// protocol type and nothing else.
    ///
    /// The protocols named here are added to that default rather than
    /// replacing it. A caller that names `cli` is asking for `cli` *as
    /// well* as what the manual could already do; a caller that wants
    /// only `cli` has a provider whose manual is not over HTTP.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::adapters::secondary::utcp::ManualClient;
    ///
    /// let client = ManualClient::new("https://api.example.com")?
    ///     .allowing_protocols(["mcp".to_owned()]);
    /// let permitted = client.registration()
    ///     .allowed_communication_protocols
    ///     .clone()
    ///     .unwrap_or_default();
    /// assert_eq!(permitted, ["http".to_owned(), "mcp".to_owned()]);
    /// # Ok::<(), protocol_droid::adapters::secondary::utcp::UtcpError>(())
    /// ```
    #[must_use]
    pub fn allowing_protocols<I, S>(mut self, protocols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let own = self.registration.call_template_type().to_owned();
        let mut permitted = vec![own];
        for protocol in protocols {
            let protocol = protocol.into();
            if !permitted.contains(&protocol) {
                permitted.push(protocol);
            }
        }
        self.registration.allowed_communication_protocols = Some(permitted);
        self
    }

    /// How long this client waits for the manual.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Fetches and reads the manual.
    ///
    /// # Errors
    ///
    /// [`UtcpError::Endpoint`] when the URL is not one this build connects
    /// to, [`UtcpError::Redirect`] when a redirect is not followed,
    /// [`UtcpError::Timeout`] when the peer does not answer in time,
    /// [`UtcpError::Transport`] when the request fails,
    /// [`UtcpError::Status`] when the peer answers with a status that is not
    /// a success, [`UtcpError::MediaType`] when the answer is not JSON, and
    /// [`UtcpError::Manual`] when the document is not a manual.
    pub async fn fetch(&self) -> Result<Manual, UtcpError> {
        let response = self.send(&self.url, 0).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(UtcpError::Status {
                status: status.as_u16(),
            });
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        if !content_type.is_empty()
            && !content_type.starts_with("application/json")
            && !content_type.starts_with("text/json")
            && !content_type.contains("+json")
        {
            return Err(UtcpError::MediaType { found: content_type });
        }
        let text = response.text().await.map_err(|error| UtcpError::Transport {
            detail: error.to_string(),
        })?;
        Ok(Manual::parse(&text)?)
    }

    /// Sends the request, following redirects one hop at a time.
    ///
    /// Each hop is classified on its own, and the hop must be one this build
    /// will connect to. The manual's own URL is the first hop; a redirect
    /// away from a remote host into loopback is refused, because that is a
    /// remote server steering the request at the client's own services.
    async fn send(&self, url: &str, hop: usize) -> Result<reqwest::Response, UtcpError> {
        let mut current_url = url.to_owned();
        let mut current = endpoint::classify(&current_url)?;
        let mut hop = hop;
        loop {
            let response = self
                .client
                .get(&current_url)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(|error| {
                    if error.is_timeout() {
                        UtcpError::Timeout {
                            url: current_url.clone(),
                            seconds: self.timeout.as_secs(),
                        }
                    } else {
                        UtcpError::Transport {
                            detail: error.to_string(),
                        }
                    }
                })?;
            let Some(location) = redirect_target(&response) else {
                return Ok(response);
            };
            let next = endpoint::classify(&location)?;
            current
                .may_redirect_to(&next)
                .map_err(|error| UtcpError::Redirect {
                    url: location.clone(),
                    reason: error.to_string(),
                })?;
            if hop >= MAX_REDIRECTS {
                return Err(UtcpError::Redirect {
                    url: location,
                    reason: format!("there were more than {MAX_REDIRECTS} redirects"),
                });
            }
            hop += 1;
            current_url = location;
            current = next;
        }
    }
}

/// The location a redirect names, resolved against the URL it came from.
fn redirect_target(response: &reqwest::Response) -> Option<String> {
    let location = response.headers().get(reqwest::header::LOCATION)?;
    let location = location.to_str().ok()?;
    let base = url::Url::parse(response.url().as_str()).ok()?;
    Some(base.join(location).ok()?.to_string())
}

/// The endpoint the manual was read from.
///
/// A manual's tools are only reachable where the specification allows, and
/// that depends on where the manual came from: a remote manual may not name
/// a loopback tool URL.
///
/// # Errors
///
/// [`UtcpError::Endpoint`] when the URL is not one this build connects to.
pub fn manual_endpoint(url: &str) -> Result<Endpoint, UtcpError> {
    endpoint::classify(url).map_err(UtcpError::Endpoint)
}

#[cfg(test)]
mod tests {
    use super::ManualClient;
    use crate::adapters::secondary::utcp::error::UtcpError;

    /// The specification's path is appended to a bare origin.
    #[test]
    fn the_specifications_path_is_appended() -> Result<(), UtcpError> {
        let client = ManualClient::new("https://api.example.com")?;
        assert_eq!(client.url(), "https://api.example.com/utcp");
        Ok(())
    }

    /// A trailing slash does not double the separator.
    #[test]
    fn a_trailing_slash_does_not_double() -> Result<(), UtcpError> {
        let client = ManualClient::new("https://api.example.com/")?;
        assert_eq!(client.url(), "https://api.example.com/utcp");
        Ok(())
    }

    /// A URL that already names the path is left alone.
    #[test]
    fn a_url_that_already_names_the_path_is_left_alone() -> Result<(), UtcpError> {
        let client = ManualClient::new("https://api.example.com/utcp")?;
        assert_eq!(client.url(), "https://api.example.com/utcp");
        Ok(())
    }

    /// A plain-HTTP URL to a remote host is refused: the manual would travel
    /// in the clear.
    #[test]
    fn a_plain_http_remote_url_is_refused() {
        let refused = ManualClient::new("http://api.example.com");
        assert!(matches!(refused, Err(UtcpError::Endpoint(_))));
    }

    /// A plain-HTTP URL to a loopback address is allowed: that is a
    /// provider on this machine.
    #[test]
    fn a_plain_http_loopback_url_is_allowed() -> Result<(), UtcpError> {
        let client = ManualClient::new("http://127.0.0.1:3000")?;
        assert_eq!(client.url(), "http://127.0.0.1:3000/utcp");
        Ok(())
    }

    /// The registration permits only HTTP tools until the caller says
    /// otherwise.
    #[test]
    fn the_registration_permits_only_http_by_default() -> Result<(), UtcpError> {
        let client = ManualClient::new("https://api.example.com")?;
        let policy = crate::domain::utcp::policy::ProtocolPolicy::for_registration(
            client.registration(),
        );
        assert!(policy.permits("http"));
        assert!(!policy.permits("mcp"));
        assert!(!policy.permits("cli"));
        Ok(())
    }

    /// The caller may name the protocols the manual's tools may use.
    ///
    /// The manual's own protocol type stays permitted: a caller naming
    /// `mcp` is asking for `mcp` as well, not instead.
    #[test]
    fn the_caller_may_name_the_permitted_protocols() -> Result<(), UtcpError> {
        let client = ManualClient::new("https://api.example.com")?
            .allowing_protocols(["mcp".to_owned()]);
        let policy = crate::domain::utcp::policy::ProtocolPolicy::for_registration(
            client.registration(),
        );
        assert!(policy.permits("http"));
        assert!(policy.permits("mcp"));
        assert!(!policy.permits("cli"));
        Ok(())
    }
}
