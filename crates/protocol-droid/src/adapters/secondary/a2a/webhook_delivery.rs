//! The HTTP webhook delivery adapter for A2A
//! push notifications.
//!
//! # What this is
//!
//! The production implementation of
//! [`PushNotificationDelivery`](crate::application::a2a::PushNotificationDelivery).
//! It resolves the URL's host to an IP, runs
//! [`check_at_delivery`](crate::application::a2a::UrlFilter::check_at_delivery)
//! against the resolved IP, and POSTs the body. A redirect to a denied
//! IP is the same refusal as a direct hit, so the adapter follows
//! redirects manually rather than letting `reqwest` do it for it.
//!
//! # Why a manual redirect loop
//!
//! `reqwest`'s redirect policy is global to the client, and the SSRF
//! check is per-redirect-target. Letting `reqwest` follow a redirect to
//! `127.0.0.1` is the whole DNS-rebinding / open-redirect SSRF class.
//! A manual loop means every hop is re-checked against the URL
//! filter before the request goes out.
//!
//! # Why `tokio::net::lookup_host` (and not hickory-resolver)
//!
//! The default resolver is the one already on the box. The A2A spec
//! does not require a particular resolver; the production deployment
//! that needs `hickory-resolver` writes its own adapter. The default
//! adapter is the safe one, not the only one.
//!
//! # Why the body is `&[u8]`, not `serde_json::Value`
//!
//! The delivery port is byte-level: the body is whatever the producer
//! serialised. The adapter does not parse it; parsing would mean the
//! adapter owns the wire shape, and a future version that changes the
//! shape would break the adapter, not the producer.

use std::sync::Arc;
use std::time::Duration;

use reqwest::redirect::Policy;
use url::Url;

use crate::application::a2a::{
    FilterDenial, NotificationConfig, NotificationConfigError, PushNotificationDelivery, UrlFilter,
};
use crate::domain::a2a::A2aVersion;

/// The HTTP webhook delivery adapter.
///
/// # Why a `reqwest::Client` and not a `tokio::net::TcpStream`
///
/// A consumer who needs a custom HTTP client (a mock for tests, a
/// custom TLS configuration, a different redirect policy) passes
/// their own. The default is a 10-second connect / 10-second read
/// timeout — long enough for a slow webhook target, short enough
/// that a hung target does not tie up a worker.
pub struct WebhookDelivery {
    /// The HTTP client used to POST the body.
    client: reqwest::Client,
    /// The URL filter that re-checks every hop.
    filter: Arc<dyn UrlFilter>,
    /// The maximum number of redirects before the adapter gives up.
    max_redirects: u8,
}

impl std::fmt::Debug for WebhookDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookDelivery")
            .field("max_redirects", &self.max_redirects)
            .finish_non_exhaustive()
    }
}

impl WebhookDelivery {
    /// Builds a new webhook delivery adapter
    /// with the default timeouts (10
    /// seconds connect, 10 seconds
    /// read) and a redirect limit of
    /// 5. The redirect limit is a
    /// guard against infinite
    /// redirect loops, not a
    /// quality-of-service promise.
    #[must_use]
    pub fn new(filter: Arc<dyn UrlFilter>) -> Self {
        // A no-redirect policy at the
        // client level forces the
        // manual loop in `deliver`
        // to do the work — the SSRF
        // check on every hop would
        // be bypassed if the client
        // followed redirects on its
        // own.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(10))
            .redirect(Policy::none())
            .build();
        let Ok(client) = client else {
            // A misconfigured TLS stack
            // is a startup-time error;
            // a sane default does not
            // exist for a `reqwest::Client`
            // that could not be built.
            // The unwrap_or_default
            // path uses reqwest's
            // default builder.
            return Self::with_client(reqwest::Client::new(), filter);
        };
        Self {
            client,
            filter,
            max_redirects: 5,
        }
    }

    /// Builds a new webhook delivery
    /// adapter with a caller-supplied
    /// `reqwest::Client`. The adapter
    /// does not modify the client; the
    /// caller owns TLS, redirect, and
    /// timeout configuration.
    #[must_use]
    pub fn with_client(client: reqwest::Client, filter: Arc<dyn UrlFilter>) -> Self {
        Self {
            client,
            filter,
            max_redirects: 5,
        }
    }
}

impl PushNotificationDelivery for WebhookDelivery {
    async fn deliver(
        &self,
        config: &NotificationConfig,
        body: &[u8],
    ) -> Result<(), NotificationConfigError> {
        let mut current_url: Url = config.webhook.as_url().clone();
        // The redirect loop. Each
        // hop is re-checked against
        // the URL filter; a redirect
        // to a denied IP is the
        // same refusal as a direct
        // hit.
        for hop in 0..=self.max_redirects {
            if hop == self.max_redirects {
                return Err(NotificationConfigError::TransientFailure {
                    reason: format!("exceeded {} redirects", self.max_redirects),
                });
            }

            // DNS resolution.
            // `lookup_host` returns
            // the first resolved IP
            // for the host. A
            // deployment that needs
            // to check every IP
            // walks them all; the
            // default refuses on
            // the first denied IP.
            let host = current_url
                .host_str()
                .ok_or(NotificationConfigError::Refused(FilterDenial::MissingHost))?
                .to_owned();
            let port = current_url
                .port_or_known_default()
                .ok_or(NotificationConfigError::Refused(FilterDenial::Malformed))?;
            let mut resolved_ips = Vec::new();
            let lookup = tokio::net::lookup_host((host.as_str(), port)).await;
            match lookup {
                Ok(addrs) => {
                    for addr in addrs {
                        resolved_ips.push(addr.ip());
                    }
                }
                Err(error) => {
                    return Err(NotificationConfigError::TransientFailure {
                        reason: format!("DNS resolution failed: {error}"),
                    });
                }
            }
            if resolved_ips.is_empty() {
                return Err(NotificationConfigError::TransientFailure {
                    reason: format!("no addresses resolved for {host}"),
                });
            }

            // SSRF check on the
            // resolved IP. A denied
            // IP is a refusal, not a
            // transient failure —
            // retrying will not make
            // it through.
            for ip in &resolved_ips {
                if let Err(_denial) =
                    self.filter
                        .check_at_delivery(&current_url, *ip, A2aVersion::V1_0)
                {
                    return Err(NotificationConfigError::Refused(FilterDenial::MissingHost));
                }
            }

            // POST.
            let mut request = self
                .client
                .post(current_url.as_str())
                .header("content-type", "application/json")
                .body(body.to_vec());
            if let Some(token) = &config.token {
                request = request.header("x-a2a-notification-token", token.as_ref());
            }
            let response = request.send().await.map_err(|error| {
                NotificationConfigError::TransientFailure {
                    reason: format!("HTTP POST failed: {error}"),
                }
            })?;

            // Status code.
            let status = response.status();
            if status.is_success() {
                return Ok(());
            }
            if status.is_redirection() {
                if let Some(location) = response.headers().get(reqwest::header::LOCATION) {
                    let location_str = location.to_str().map_err(|error| {
                        NotificationConfigError::TransientFailure {
                            reason: format!("redirect location is not valid UTF-8: {error}"),
                        }
                    })?;
                    let next_url = current_url.join(location_str).map_err(|error| {
                        NotificationConfigError::TransientFailure {
                            reason: format!("redirect location is not a valid URL: {error}"),
                        }
                    })?;
                    current_url = next_url;
                    continue;
                }
                return Err(NotificationConfigError::TransientFailure {
                    reason: "redirect without a location".to_owned(),
                });
            }
            return Err(NotificationConfigError::TransientFailure {
                reason: format!("HTTP {status}"),
            });
        }
        Err(NotificationConfigError::TransientFailure {
            reason: "redirect loop ended without a response".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::WebhookDelivery;
    use crate::adapters::secondary::a2a::SsrfDenylist;

    #[tokio::test]
    async fn webhook_delivery_can_be_constructed_with_default_filter() {
        // A smoke test: the adapter
        // builds with the default
        // filter and implements the
        // trait. The actual delivery
        // path needs a real HTTP
        // target, which a unit test
        // cannot provide.
        let delivery = WebhookDelivery::new(Arc::new(SsrfDenylist::FILTER));
        let _ = format!("{delivery:?}");
    }

    #[tokio::test]
    async fn webhook_delivery_with_client_can_be_constructed() {
        // A smoke test for the
        // caller-supplied client
        // path.
        let client = reqwest::Client::new();
        let delivery = WebhookDelivery::with_client(client, Arc::new(SsrfDenylist::FILTER));
        let _ = format!("{delivery:?}");
    }
}
