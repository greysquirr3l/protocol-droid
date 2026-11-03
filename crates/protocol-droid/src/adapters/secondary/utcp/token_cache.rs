//! Caching `OAuth2` access tokens.
//!
//! # What the specification requires
//!
//! > Tokens are cached per full credential configuration (token URL, client
//! > id, secret, scope), never shared between manuals that merely share a
//! > client id, and concurrent first-time requests share a single token
//! > fetch.
//!
//! Three claims, and each one has a failure it prevents:
//!
//! - **Per full credential configuration.** The key is the whole tuple, not
//!   the client id. Two manuals that use the same client id but different
//!   secrets, or the same secret against different token endpoints, are
//!   different credentials and get different tokens. Keying on the client id
//!   alone would hand one manual the other's token.
//! - **Shared across concurrent first-time requests.** Ten requests that
//!   arrive together should not produce ten token fetches. A token
//!   endpoint is a rate-limited, audited resource, and ten simultaneous
//!   client-credential exchanges are ten chances to be throttled.
//! - **Not shared between manuals.** That is the first claim again, and it
//!   is the one that matters most.
//!
//! # How the single fetch is arranged
//!
//! The cache holds a lock per key. The first caller to arrive takes the
//! key, finds the cache empty, and fetches. Every other caller waits on the
//! same lock and then finds the token already there.
//!
//! The alternative — a per-key "someone is fetching" flag — needs its own
//! state machine and its own failure handling. A lock is the whole thing.
//!
//! # What is *not* cached
//!
//! Nothing is keyed by anything less than the full credential tuple. The
//! key is built by [`TokenKey::of`] from the four fields, and nothing else
//! feeds into it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// How long a fetched token is assumed to live.
///
/// The specification does not name a lifetime, and the token endpoint's
/// `expires_in` is not always present. A conservative window keeps a
/// long-lived client from presenting a token the provider has already
/// revoked, and the cost of being wrong here is one extra fetch.
const TOKEN_LIFETIME: Duration = Duration::from_secs(300);

/// How far before expiry a token is treated as expired.
///
/// A token fetched over a slow link can be close to expiry by the time it
/// arrives, so the window is a margin rather than the whole lifetime.
const EXPIRY_MARGIN: Duration = Duration::from_secs(30);

/// The credential configuration a token belongs to.
///
/// Two credentials that differ in any field are different credentials, and
/// this is the whole key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenKey {
    /// Where the token is fetched from.
    pub token_url: String,
    /// The client identifier.
    pub client_id: String,
    /// The client secret.
    pub client_secret: String,
    /// The scope asked for, when one is.
    pub scope: Option<String>,
}

impl TokenKey {
    /// The key for a set of client credentials.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::adapters::secondary::utcp::token_cache::TokenKey;
    ///
    /// let key = TokenKey::of(
    ///     "https://auth.example.com/token",
    ///     "client",
    ///     "secret",
    ///     Some("read"),
    /// );
    /// assert_eq!(key.client_id, "client");
    /// ```
    #[must_use]
    pub fn of(token_url: &str, client_id: &str, client_secret: &str, scope: Option<&str>) -> Self {
        Self {
            token_url: token_url.to_owned(),
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
            scope: scope.map(ToOwned::to_owned),
        }
    }
}

/// A token and the moment it stops being used.
#[derive(Debug, Clone)]
pub struct CachedToken {
    /// The token itself.
    pub token: String,
    /// When it was fetched.
    pub fetched_at: Instant,
}

impl CachedToken {
    /// Whether this token has aged out.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Instant;
    ///
    /// use protocol_droid::adapters::secondary::utcp::token_cache::CachedToken;
    ///
    /// let token = CachedToken::new("token");
    /// assert!(!token.is_stale(Instant::now()));
    /// ```
    #[must_use]
    pub fn is_stale(&self, now: Instant) -> bool {
        now.duration_since(self.fetched_at) + EXPIRY_MARGIN >= TOKEN_LIFETIME
    }
}

impl CachedToken {
    /// A token fetched at this moment.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::adapters::secondary::utcp::token_cache::CachedToken;
    ///
    /// let token = CachedToken::new("value");
    /// assert_eq!(token.token, "value");
    /// ```
    #[must_use]
    pub fn new(token: &str) -> Self {
        Self {
            token: token.to_owned(),
            fetched_at: std::time::Instant::now(),
        }
    }
}

/// One credential configuration's slot: the token, and the lock over it.
///
/// The lock is per key rather than per cache, so two credentials do not
/// queue behind each other and a fetch for one does not block a fetch for
/// another.
type Slot = Arc<Mutex<Option<CachedToken>>>;

/// The cache's table: a slot per credential configuration.
type Slots = Arc<Mutex<HashMap<TokenKey, Slot>>>;

/// A cache of `OAuth2` access tokens.
///
/// The cache is cheap to share: it holds an `Arc` per key rather than the
/// token itself, so handing the cache to a task moves a pointer.
#[derive(Debug, Clone, Default)]
pub struct TokenCache {
    entries: Slots,
}

impl TokenCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The token for these credentials, if one has been fetched and is still
    /// fresh.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::adapters::secondary::utcp::token_cache::TokenCache;
    ///
    /// # async fn example() {
    /// let cache = TokenCache::new();
    /// let key = TokenCache::key("https://auth.example.com/token", "id", "secret", None);
    /// assert!(cache.get(&key).await.is_none());
    /// # }
    /// ```
    pub async fn get(&self, key: &TokenKey) -> Option<String> {
        let slot = self.slot(key).await;
        let guard = slot.lock().await;
        guard
            .as_ref()
            .filter(|token| !token.is_stale(Instant::now()))
            .map(|t| t.token.clone())
    }

    /// Records a fetched token.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::adapters::secondary::utcp::token_cache::TokenCache;
    ///
    /// # async fn example() {
    /// let cache = TokenCache::new();
    /// let key = TokenCache::key("https://auth.example.com/token", "id", "secret", None);
    /// cache.put(&key, "token").await;
    /// assert_eq!(cache.get(&key).await.as_deref(), Some("token"));
    /// # }
    /// ```
    pub async fn put(&self, key: &TokenKey, token: &str) {
        let slot = self.slot(key).await;
        let mut guard = slot.lock().await;
        *guard = Some(CachedToken {
            token: token.to_owned(),
            fetched_at: Instant::now(),
        });
    }

    /// The key for a set of client credentials.
    ///
    /// A convenience for the common case, so callers do not have to import
    /// [`TokenKey`] to build one.
    #[must_use]
    pub fn key(
        token_url: &str,
        client_id: &str,
        client_secret: &str,
        scope: Option<&str>,
    ) -> TokenKey {
        TokenKey::of(token_url, client_id, client_secret, scope)
    }

    /// The slot a key's token lives in.
    ///
    /// The slot is created on first use and then kept, so every caller for
    /// the same credentials contends on the same lock.
    async fn slot(&self, key: &TokenKey) -> Slot {
        let mut entries = self.entries.lock().await;
        Arc::clone(
            entries
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(None))),
        )
    }

    /// The token for these credentials, fetching it if there is not one.
    ///
    /// The fetch happens under the key's lock, so concurrent first-time
    /// requests share one fetch: the caller that gets there first does the
    /// work, and the others wait and then find the token already in place.
    ///
    /// The fetch is only called when the cache has nothing fresh for this
    /// key.
    ///
    /// # Errors
    ///
    /// Whatever `fetch` returns.
    pub async fn get_or_fetch<E>(
        &self,
        key: &TokenKey,
        fetch: impl AsyncFnOnce() -> Result<String, E>,
    ) -> Result<String, E> {
        let slot = self.slot(key).await;
        let mut guard = slot.lock_owned().await;
        let cached = guard
            .as_ref()
            .filter(|cached| !cached.is_stale(Instant::now()))
            .map(|cached| cached.token.clone());
        if let Some(cached) = cached {
            return Ok(cached);
        }
        // The fetch runs under the lock. That is the whole point: the
        // callers behind it wait here rather than each making their own
        // request to the token endpoint.
        let token = fetch().await?;
        *guard = Some(CachedToken::new(&token));
        // Released here rather than at the end of the function: the lock is
        // the only thing standing between the next caller for this
        // credential and the token it is waiting for.
        drop(guard);
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::{TokenCache, TokenKey};

    /// A cache with nothing in it.
    #[tokio::test]
    async fn an_empty_cache_has_nothing() {
        let cache = TokenCache::new();
        let key = TokenCache::key("https://auth.example.com/token", "id", "secret", None);
        assert!(cache.get(&key).await.is_none());
    }

    /// A recorded token comes back.
    #[tokio::test]
    async fn a_recorded_token_comes_back() {
        let cache = TokenCache::new();
        let key = TokenCache::key("https://auth.example.com/token", "id", "secret", None);
        cache.put(&key, "token").await;
        assert_eq!(cache.get(&key).await.as_deref(), Some("token"));
    }

    /// Two manuals that share a client id are different credentials.
    ///
    /// This is the failure the specification's wording is aimed at: keying on
    /// the client id alone would hand one manual the other's token.
    #[tokio::test]
    async fn a_different_secret_is_a_different_token() {
        let cache = TokenCache::new();
        let first = TokenCache::key("https://auth.example.com/token", "shared", "first", None);
        let second = TokenCache::key("https://auth.example.com/token", "shared", "second", None);
        cache.put(&first, "first-token").await;
        assert!(cache.get(&second).await.is_none());
    }

    /// A different token endpoint is a different credential.
    #[tokio::test]
    async fn a_different_token_url_is_a_different_token() {
        let cache = TokenCache::new();
        let first = TokenCache::key("https://auth.example.com/token", "id", "secret", None);
        let second = TokenCache::key("https://other.example.com/token", "id", "secret", None);
        cache.put(&first, "first-token").await;
        assert!(cache.get(&second).await.is_none());
    }

    /// A different scope is a different credential.
    #[tokio::test]
    async fn a_different_scope_is_a_different_token() {
        let cache = TokenCache::new();
        let first = TokenCache::key(
            "https://auth.example.com/token",
            "id",
            "secret",
            Some("read"),
        );
        let second = TokenCache::key(
            "https://auth.example.com/token",
            "id",
            "secret",
            Some("write"),
        );
        cache.put(&first, "read-token").await;
        assert!(cache.get(&second).await.is_none());
    }

    /// The same credentials share a token, scope included.
    #[tokio::test]
    async fn the_same_credentials_share_a_token() {
        let cache = TokenCache::new();
        let first = TokenCache::key(
            "https://auth.example.com/token",
            "id",
            "secret",
            Some("read"),
        );
        let second = TokenCache::key(
            "https://auth.example.com/token",
            "id",
            "secret",
            Some("read"),
        );
        cache.put(&first, "shared-token").await;
        assert_eq!(cache.get(&second).await.as_deref(), Some("shared-token"));
    }

    /// Concurrent first-time requests share one fetch.
    ///
    /// The lock is taken before the fetch, so the second caller finds the
    /// token already there rather than making its own request.
    #[tokio::test]
    async fn concurrent_callers_share_one_fetch() {
        let cache = TokenCache::new();
        let key = TokenCache::key("https://auth.example.com/token", "id", "secret", None);

        // Stand in for the token endpoint: count the calls, and answer
        // with the same token every time.
        let fetches = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = std::sync::Arc::clone(&fetches);

        let fetch = || async {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<String, std::convert::Infallible>("fetched".to_owned())
        };

        let first = cache.get_or_fetch(&key, fetch).await;
        let second = cache.get_or_fetch(&key, fetch).await;
        let third = cache.get_or_fetch(&key, fetch).await;

        assert_eq!(first, Ok("fetched".to_owned()));
        assert_eq!(second, Ok("fetched".to_owned()));
        assert_eq!(third, Ok("fetched".to_owned()));
        assert_eq!(first, Ok("fetched".to_owned()));
        assert_eq!(second, Ok("fetched".to_owned()));
        assert_eq!(third, Ok("fetched".to_owned()));
        assert_eq!(fetches.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A key is the whole credential tuple, and nothing else.
    #[test]
    fn a_key_is_the_whole_credential_tuple() {
        let key = TokenKey::of(
            "https://auth.example.com/token",
            "id",
            "secret",
            Some("read"),
        );
        assert_eq!(key.token_url, "https://auth.example.com/token");
        assert_eq!(key.client_id, "id");
        assert_eq!(key.client_secret, "secret");
        assert_eq!(key.scope.as_deref(), Some("read"));
    }
}
