//! A2A push-notification delivery — the use case the wire codec feeds.
//!
//! # Why a port and not a function
//!
//! Delivering a push notification is two things at once:
//!
//! - A *policy* check (the URL filter, OWASP API7) — pure, testable, the
//!   same on every deployment.
//! - A *delivery* step (open a socket, POST a body, retry on failure) —
//!   infrastructure, swappable, deployment-specific.
//!
//! Mixing them in one trait would put a `reqwest::Client` into a port a
//! domain-logic handler has to depend on. Splitting them lets the policy
//! stay in `domain/` and `application/`, and the delivery live in
//! `adapters/secondary/a2a/`. The `PushNotificationDelivery` port here is
//! the integration point — the wire codec hands the config and the payload
//! in, the port returns success or a typed refusal, and the rest of the
//! handler is none the wiser about which HTTP client is on the other side.
//!
//! # Idempotency
//!
//! The A2A spec does not name a delivery-attempt header (e.g. MCP's
//! `Mcp-Session-Id`); what it does say is that the same notification may
//! be delivered more than once when a previous attempt did not
//! acknowledge. The [`WebhookUrl::idempotency_key`] is the consumer's
//! chance to dedupe: implementations should record `(idempotency_key,
//! response)` and return the recorded response on retry.

use std::sync::Arc;

use url::Url;

use crate::application::a2a::UrlFilter;
use crate::domain::a2a::A2aVersion;
use crate::domain::idempotency::IdempotencyKey;

/// The push-notification URL a client registered, after URL filter approval.
///
/// The newtype exists so the wire codec can parse a `Url` once, hand the
/// filtered value to the use case, and have the type system prove the URL
/// was checked before delivery. A `Url` that arrives at the delivery port
/// without going through [`WebhookUrl::new`] is a delivery path that
/// bypassed the SSRF guard.
#[derive(Debug, Clone)]
pub struct WebhookUrl(Url);

impl WebhookUrl {
    /// Wraps a URL that has already passed the [`UrlFilter::check_at_config`]
    /// gate.
    ///
    /// # Errors
    ///
    /// The only thing this constructor can refuse is a URL that the filter
    /// would refuse — [`UrlFilter::check_at_config`] is the gate, not this
    /// constructor, but the constructor delegates the check to the
    /// provided filter so the wire codec has one call to make and one
    /// error to handle.
    pub fn new(
        filter: &dyn UrlFilter,
        url: Url,
        version: A2aVersion,
    ) -> Result<Self, super::FilterDenial> {
        filter.check_at_config(&url, version)?;
        Ok(Self(url))
    }

    /// The inner URL, for an adapter that has to make the request.
    #[must_use]
    pub const fn as_url(&self) -> &Url {
        &self.0
    }

    /// The key the delivery adapter uses to dedupe retried deliveries.
    ///
    /// `WebhookUrl::idempotency_key(url)` is a pure function of the URL —
    /// the same URL produces the same key on every call. The delivery
    /// adapter records `(key, response)` and returns the recorded
    /// response on retry, so a network blip that lost the `2xx` reply
    /// does not produce a duplicate notification.
    #[must_use]
    pub fn idempotency_key(&self) -> IdempotencyKey {
        // The URL is the deduplication scope. Two delivery attempts for
        // the same URL on the same task are the same notification; the
        // adapter's `(task_id, url)` record catches cross-task duplicates
        // as a side effect of the URL being unique per task in the spec.
        IdempotencyKey::new(self.0.as_str())
    }
}

/// A `PushNotificationConfig` the A2A client registered, post-validation.
///
/// The wire codec decodes the JSON, the URL filter clears the URL, and
/// the resulting `NotificationConfig` is what the delivery port sees.
#[derive(Debug, Clone)]
pub struct NotificationConfig {
    /// The webhook the server POSTs to.
    pub webhook: WebhookUrl,
    /// An opaque token the server includes in the `X-A2A-Notification-Token`
    /// header so the receiver can authenticate the delivery. Optional in
    /// the A2A spec; a receiver without a token is a receiver that does
    /// not need to authenticate, and the spec does not require one.
    pub token: Option<Arc<str>>,
}

/// Failure modes for a delivery attempt, in priority order.
///
/// `Refused` (the filter denied the URL) is distinct from
/// `TransientFailure` (the filter accepted the URL but the network
/// refused) so the handler can choose to surface one and silently retry
/// the other. The split is what makes the difference between "the client
/// is malicious" and "the network is bad" a typed answer.
#[derive(Debug, thiserror::Error)]
pub enum NotificationConfigError {
    /// The URL was rejected at config time. Surfacing this lets the
    /// handler answer the client with the typed refusal; retrying will
    /// not help.
    #[error("the webhook URL was rejected: {0}")]
    Refused(#[from] super::url_filter::FilterDenial),
    /// The URL was accepted but the
    /// delivery step failed (network,
    /// timeout, non-2xx, or a
    /// redirect loop). The handler
    /// decides whether to retry;
    /// the port reports the failure
    /// as a structured reason.
    #[error("transient delivery failure: {reason}")]
    TransientFailure {
        /// A human-readable reason for the failure.
        reason: String,
    },
}

/// The port the push-notification handler depends on.
///
/// An implementation is `HttpPushNotificationDelivery` (in
/// `adapters/secondary/a2a/`); a test implementation is a closure or a
/// `Vec<NotificationConfig>` in-memory recorder. The handler is none the
/// wiser.
pub trait PushNotificationDelivery: Send + Sync {
    /// Delivers one notification.
    ///
    /// # Errors
    ///
    /// - [`NotificationConfigError::Refused`] if the URL filter rejects
    ///   the URL at delivery time (a DNS-rebind to a denied IP, for
    ///   example).
    /// - A network / timeout / non-2xx error if the POST failed and a
    ///   retry is appropriate. The handler maps this to its own retry
    ///   policy; the port deliberately returns no concrete type here so
    ///   an adapter can carry a `reqwest::Error` (or a
    ///   `tokio::io::Error`, or a custom error) without forcing the
    ///   handler to name a third-party error type.
    fn deliver(
        &self,
        config: &NotificationConfig,
        body: &[u8],
    ) -> impl std::future::Future<Output = Result<(), NotificationConfigError>> + Send;
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        reason = "tests assert on fallible calls with named failure messages; propagating errors would replace a clear failure with noise"
    )]

    use std::str::FromStr;

    use super::{NotificationConfig, NotificationConfigError, WebhookUrl};
    use crate::adapters::secondary::a2a::SsrfDenylist;
    use crate::application::a2a::PushNotificationDelivery;
    use crate::domain::a2a::A2aVersion;

    type TestError = Box<dyn std::error::Error>;

    const V1_0: A2aVersion = A2aVersion::V1_0;

    #[test]
    fn idempotency_key_is_deterministic() -> Result<(), TestError> {
        // The same URL must produce the same key on every call, or the
        // delivery adapter's retry-dedup record misses a duplicate.
        let url: url::Url = "https://example.com/hook".parse()?;
        let a = WebhookUrl::new(&SsrfDenylist::FILTER, url.clone(), V1_0)
            .expect("the public HTTPS URL must be accepted by the default filter");
        let b = WebhookUrl::new(&SsrfDenylist::FILTER, url, V1_0)
            .expect("the same URL must be accepted on a second call");
        assert_eq!(
            a.idempotency_key().as_str(),
            b.idempotency_key().as_str(),
            "the same URL must produce the same key on every call"
        );

        // A different URL must produce a different key, or the adapter
        // would mistake one notification for another.
        let other: url::Url = "https://example.com/hook?x=1".parse()?;
        let c = WebhookUrl::new(&SsrfDenylist::FILTER, other, V1_0)
            .expect("a different public URL must be accepted");
        assert_ne!(
            a.idempotency_key().as_str(),
            c.idempotency_key().as_str(),
            "different URLs must produce different keys"
        );
        Ok(())
    }

    #[test]
    fn webhook_url_rejects_a_filter_denied_url() {
        // 127.0.0.1 is on the default denylist. The constructor
        // delegates to `check_at_config`, so a loopback URL cannot
        // become a `WebhookUrl`.
        let url = url::Url::from_str("http://127.0.0.1/hook")
            .expect("the test fixture must parse as a URL");
        let err = WebhookUrl::new(&SsrfDenylist::FILTER, url, V1_0)
            .expect_err("a loopback URL must be denied at the constructor");
        let matched = matches!(
            err,
            crate::application::a2a::FilterDenial::DeniedIpLiteral { .. }
        );
        assert!(matched, "denial was {err:?}");
    }

    #[test]
    fn notification_config_wraps_a_filtered_webhook() -> Result<(), TestError> {
        // The wire codec decodes a `PushNotificationConfig`, runs the
        // URL through the filter, and hands the application layer a
        // `NotificationConfig`. The path through the public API is
        // small enough to exercise in a single test.
        let url: url::Url = "https://example.com/hook".parse()?;
        let webhook = WebhookUrl::new(&SsrfDenylist::FILTER, url, V1_0)?;
        let config = NotificationConfig {
            webhook,
            token: None,
        };
        assert_eq!(config.webhook.as_url().scheme(), "https");
        assert!(config.token.is_none());
        Ok(())
    }

    #[test]
    fn notification_config_error_preserves_the_filter_denial() -> Result<(), TestError> {
        // The `Refused` variant wraps the typed `FilterDenial` so a
        // handler can surface the *class* of refusal (loopback, RFC
        // 1918, scheme, etc.) rather than collapsing every config
        // error into "the URL was rejected".
        use crate::application::a2a::FilterDenial;
        let denial = FilterDenial::DeniedIpLiteral {
            literal: "10.0.0.1".to_owned(),
        };
        let err = NotificationConfigError::Refused(denial.clone());
        let display = format!("{err}");
        assert!(display.contains("rejected"), "display was {display}");
        match err {
            NotificationConfigError::Refused(d) => assert_eq!(d, denial),
            NotificationConfigError::TransientFailure { reason } => {
                return Err(format!("expected Refused, got TransientFailure({reason})").into());
            }
        }
        Ok(())
    }

    /// A recording impl of `PushNotificationDelivery` for the trait test.
    ///
    /// The point of the test is to prove the trait is consumable by a
    /// downstream impl — a closed test that does not name the trait
    /// would leave the public surface unexercised.
    #[derive(Debug, Default)]
    struct RecordingDelivery {
        delivered: std::sync::Mutex<Vec<Vec<u8>>>,
    }

    impl PushNotificationDelivery for RecordingDelivery {
        // The recording impl has nothing to await. Returning
        // `std::future::ready` matches the trait's RPITIT signature
        // (`fn deliver(&self) -> impl Future<...>`) without the
        // `async fn` keyword, so `clippy::unused_async` does not
        // fire and the test stays an honest statement of what a
        // sync impl looks like.
        fn deliver(
            &self,
            _config: &NotificationConfig,
            body: &[u8],
        ) -> impl std::future::Future<Output = Result<(), NotificationConfigError>> + Send {
            let outcome = self.delivered.lock().map_or_else(
                |_| {
                    Err(NotificationConfigError::Refused(
                        crate::application::a2a::FilterDenial::Malformed,
                    ))
                },
                |mut v| {
                    v.push(body.to_vec());
                    Ok(())
                },
            );
            std::future::ready(outcome)
        }
    }

    #[test]
    fn push_notification_delivery_is_implementable() -> Result<(), TestError> {
        // The trait has the right shape: a downstream consumer can
        // write a struct, implement it, and have the async method
        // call through. This pins the trait's signature. The trait
        // uses native RPITIT (return-position `impl Trait`), so a
        // `Box<dyn PushNotificationDelivery>` is not the right shape
        // here — the test exercises the concrete impl, which is the
        // form a hot-path port uses at the call site.
        let delivery = RecordingDelivery::default();
        let url: url::Url = "https://example.com/hook".parse()?;
        let webhook = WebhookUrl::new(&SsrfDenylist::FILTER, url, V1_0)?;
        let config = NotificationConfig {
            webhook,
            token: None,
        };
        // Drive the trait's `deliver` method through a small executor
        // so the test exercises the impl, not just the type. The dev
        // dependency on `tokio` is for the test runtime; production
        // code never depends on a particular executor.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| -> Box<dyn std::error::Error> { format!("{e:?}").into() })?;
        let body = b"hello";
        let outcome: Result<(), NotificationConfigError> =
            runtime.block_on(delivery.deliver(&config, body));
        outcome.map_err(|e| -> Box<dyn std::error::Error> { format!("{e:?}").into() })?;
        let recorded = delivery
            .delivered
            .lock()
            .map_err(|e| -> Box<dyn std::error::Error> { format!("{e:?}").into() })?
            .len();
        assert_eq!(recorded, 1);
        Ok(())
    }
}
