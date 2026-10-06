//! Choosing the key a presented token was signed with.
//!
//! This is where the JWT attack surface is, and almost all of it is about
//! *which key gets used* rather than about the signature maths. A signature
//! check answers "was this signed by the holder of key K"; every question worth
//! asking about K is asked before it, here.
//!
//! # A key never comes from the token
//!
//! JWS carries an embedded `jwk` header (RFC 7515 §4.1.3), and it is
//! deliberately ignored. Honouring it would make verification a fixed point: an
//! attacker signs a token with a key they generated, embeds it, and this crate
//! verifies against the attacker's own key. The only key that may verify a
//! token is one this server fetched from the configured `jwks_uri`.
//!
//! # Four independent gates
//!
//! Each of these closes a different hole, and none of them subsumes another:
//!
//! 1. **`alg` must be on the accept-list.** RFC 8725 §3.1: an unlisted
//!    algorithm is refused, and `none` is never on any list this crate builds.
//!    Without this, a token with `alg: none` has an empty signature and would
//!    verify against an empty key.
//! 2. **The key's own declared `alg` must agree.** RFC 7517 §4.4. A key
//!    published as RS256 is for RS256; using it for RS512 is a downgrade and
//!    using it for HS256 is the classic algorithm-confusion attack.
//! 3. **`use` must be `sig`, and `key_ops` must include `verify`.** RFC 7517
//!    §4.2–4.3. A key published as `enc` is an encryption key, and a
//!    deployment that publishes both from one endpoint did not mean for the
//!    encryption half to authenticate anyone.
//! 4. **The key's *type* must match the algorithm's family.** `DecodingKey`
//!    enforces this too, but the check happens after key material has been
//!    loaded, and saying it here means the pairing is testable on its own.
//!
//! # Rotation, and the flood that rotation invites
//!
//! An unknown `kid` triggers exactly one refetch, which is what makes key
//! rotation work without a restart. It is rate-limited: a token naming a
//! random `kid` every time would otherwise turn this into a way to make the
//! server fetch its key set on demand, forever, at an attacker's pace. The
//! floor is why [`KeySet::key_for`] takes a forced-refresh budget from its
//! caller rather than deciding to refetch on its own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use jsonwebtoken::jwk::{
    AlgorithmParameters, Jwk, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse,
};
use jsonwebtoken::{Algorithm, DecodingKey, Header};

/// Why no key could be chosen for a presented token.
///
/// Every variant is a refusal, not a malfunction: a JWKS is served by the same
/// party that minted the token, so anything wrong with the *match* is a token
/// this server should not accept. A key set that cannot be fetched at all is
/// the separate case, and is reported by the caller as a failure to ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(super) enum KeyError {
    /// The token named no `kid` and the key set holds more than one key.
    ///
    /// Guessing here is how a rotation takes an outage: picking "the first"
    /// verifies a valid token against the wrong key and reports it as a
    /// signature failure, which reads as a bug in the signer.
    #[error("the access token names no key and the published set is ambiguous")]
    NoKeyId,

    /// The `kid` is not in the published set, including after a refetch.
    #[error("the access token names a key this server does not publish")]
    UnknownKey,

    /// The key is published for encryption.
    #[error("the named key is published for encryption, not signatures")]
    NotForSignatures,

    /// The key's `key_ops` does not include `verify`.
    #[error("the named key is not published for verification")]
    NotForVerification,

    /// The key declares an algorithm, and it is not the token's.
    #[error("the named key is published for a different algorithm")]
    AlgorithmMismatch,

    /// The token's `alg` is not one this deployment accepts.
    #[error("the access token uses an algorithm this server does not accept")]
    UnacceptedAlgorithm,

    /// The key's material could not be turned into a verification key.
    #[error("the named key is not usable as a verification key")]
    Unusable,
}

/// Why no key could be chosen, or why the key set could not be read.
///
/// The split is the one that matters and the one a single enum would erase: a
/// token this server will not accept and an authorization server this server
/// could not ask are different facts, and
/// [`SourceError`](crate::application::oauth::SourceError) exists to keep them
/// apart. Reporting a failed fetch as an unknown key sends a caller to
/// re-authenticate against a server that is merely unreachable.
#[derive(Debug, thiserror::Error)]
pub(super) enum KeyFailure {
    /// No published key may verify this token.
    #[error("no published key may verify the presented token: {0}")]
    NoKey(#[from] KeyError),

    /// The key set could not be fetched or was not one.
    ///
    /// Carries a reason for the log and nothing else — in particular no
    /// response body, which a misconfigured endpoint can fill with whatever it
    /// likes, and no token, because no token has been involved.
    #[error("the published key set could not be read: {0}")]
    Unreadable(String),
}

/// The published key set, cached, and the rules for picking from it.
///
/// `ArcSwap` implements neither `Clone` nor `Debug`, so both are written by
/// hand below. The `Debug` one is not only a convenience: a derived one would
/// print the entire cached key set, which is a wall of base64 in any log line
/// that happens to hold a source.
pub(super) struct KeySet {
    uri: Arc<str>,
    http: reqwest::Client,
    /// Algorithms this deployment accepts. Never contains `none`.
    accepted: Arc<[Algorithm]>,
    /// The last successful fetch. `ArcSwap` rather than a `RwLock` because the
    /// authorization path runs on every request and must not serialise on a
    /// lock that a rotation takes once.
    cached: ArcSwap<Cached>,
    /// Shortest gap between two fetches this cache will make on its own.
    refresh_floor: Duration,
}

/// What one fetch produced, and when.
///
/// `keys` is `None` until a fetch succeeds, so a server that starts up before
/// its authorization server does not cache the failure forever.
#[derive(Debug, Clone, Default)]
struct Cached {
    keys: Option<Arc<JwkSet>>,
    fetched_at: Option<Instant>,
}

impl Clone for KeySet {
    /// Carries the cache across, rather than starting the clone empty.
    ///
    /// Two sources sharing a key set are the normal shape — one per route, or
    /// one per connection — and dropping the cache on each clone would mean a
    /// deployment with N of them fetching N times per rotation.
    fn clone(&self) -> Self {
        Self {
            uri: Arc::clone(&self.uri),
            http: self.http.clone(),
            accepted: Arc::clone(&self.accepted),
            cached: ArcSwap::from(self.cached.load_full()),
            refresh_floor: self.refresh_floor,
        }
    }
}

impl std::fmt::Debug for KeySet {
    /// Prints the configuration, never the cached keys.
    ///
    /// `keys_cached` is derived rather than a field, and the remaining fields
    /// are left out through `finish_non_exhaustive` so a future one does not
    /// silently start printing a key set.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeySet")
            .field("uri", &self.uri)
            .field("accepted", &self.accepted)
            .field("keys_cached", &self.cached.load().keys.is_some())
            .finish_non_exhaustive()
    }
}

impl KeySet {
    /// A key set published at `uri`.
    pub(super) fn new(uri: impl Into<Arc<str>>, accepted: Vec<Algorithm>) -> Self {
        Self {
            uri: uri.into(),
            http: reqwest::Client::new(),
            accepted: accepted.into(),
            cached: ArcSwap::from_pointee(Cached::default()),
            refresh_floor: Duration::from_secs(60),
        }
    }

    /// The algorithms this set will accept.
    pub(super) fn accepted(&self) -> &[Algorithm] {
        &self.accepted
    }

    /// Replaces the HTTP client, for a caller that needs a timeout or a proxy.
    pub(super) fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// Drops the cached set so the next request fetches again.
    ///
    /// For a long-lived process that has been told the authorization server
    /// rotated its keys out of band, rather than waiting out [`refresh_floor`].
    pub(super) fn invalidate(&self) {
        self.cached.store(Arc::new(Cached::default()));
    }

    /// Fetches the key set now.
    ///
    /// # Errors
    ///
    /// [`KeyFailure::Unreadable`] when the set could not be fetched or parsed.
    /// An empty set is *not* an error: an authorization server that publishes
    /// no keys has answered, and the first token to arrive is what reports it.
    pub(super) async fn warm(&self) -> Result<(), KeyFailure> {
        self.fetch().await.map(|_| ())
    }

    /// The key `header` was signed with, as a verification key.
    ///
    /// `allow_refresh` is the caller's budget for a rotation-triggered refetch,
    /// so that a burst of unknown `kid`s costs one fetch rather than one per
    /// request.
    ///
    /// # Errors
    ///
    /// [`KeyFailure::NoKey`] when no published key may verify this token, and
    /// [`KeyFailure::Unreadable`] when the set could not be read.
    pub(super) async fn key_for(
        &self,
        header: &Header,
        allow_refresh: bool,
    ) -> Result<DecodingKey, KeyFailure> {
        // Gate 1, before any key material is touched. `DecodingKey` construction
        // is cheap, but the point is that an unaccepted algorithm never reaches
        // the point where a key exists to check it against.
        if !self.accepted.contains(&header.alg) {
            return Err(KeyFailure::NoKey(KeyError::UnacceptedAlgorithm));
        }

        if let Some(keys) = self.cached.load().keys.clone() {
            match select(&keys, header) {
                Ok(jwk) => return decode(jwk, header).map_err(KeyFailure::NoKey),
                // Only an unknown `kid` is worth a refetch. An ambiguous or
                // mis-declared key is a property of the published set, and
                // re-fetching it would let a caller who controls the set decide
                // when this server believes it.
                Err(KeyError::UnknownKey) => {}
                Err(other) => return Err(other.into()),
            }
        }

        let floor = self.cached.load().fetched_at;
        let due = floor.is_none_or(|at| at.elapsed() >= self.refresh_floor);
        if !allow_refresh || !due {
            return Err(KeyError::UnknownKey.into());
        }

        let keys = self.fetch().await?;
        let jwk = select(&keys, header)?;
        decode(jwk, header).map_err(KeyFailure::NoKey)
    }

    /// Fetches the published set and installs it.
    ///
    /// The status is checked before the body is read: a non-2xx from a key
    /// endpoint is a real answer, and decoding its body as a key set would
    /// produce an empty set that reads as "this server publishes no keys"
    /// rather than "the fetch failed". An empty set that *is* a key set is
    /// kept — the authorization server answered, and the first token to arrive
    /// is what reports the consequence.
    async fn fetch(&self) -> Result<Arc<JwkSet>, KeyFailure> {
        let response = self
            .http
            .get(&*self.uri)
            .send()
            .await
            .map_err(|error| KeyFailure::Unreadable(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(KeyFailure::Unreadable(format!(
                "the key endpoint answered {status}"
            )));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| KeyFailure::Unreadable(error.to_string()))?;
        let keys: JwkSet = serde_json::from_slice(&body)
            .map_err(|error| KeyFailure::Unreadable(error.to_string()))?;
        let keys = Arc::new(keys);
        self.cached.store(Arc::new(Cached {
            keys: Some(Arc::clone(&keys)),
            fetched_at: Some(Instant::now()),
        }));
        Ok(keys)
    }
}

/// Picks the one published key that may verify `header`.
///
/// Gates 2 through 4. Split out from [`KeySet`] so each rule is testable
/// against a set that is built in the test rather than fetched.
pub(super) fn select<'set>(keys: &'set JwkSet, header: &Header) -> Result<&'set Jwk, KeyError> {
    let chosen = match &header.kid {
        Some(kid) => keys.find(kid).ok_or(KeyError::UnknownKey)?,
        // RFC 7517 §4.5 calls `kid` a hint rather than a requirement, so a set
        // of exactly one key is unambiguous without it. More than one is not,
        // and there is no ordering to break the tie with.
        None => match keys.keys.as_slice() {
            [only] => only,
            _ => return Err(KeyError::NoKeyId),
        },
    };

    if chosen.common.public_key_use == Some(PublicKeyUse::Encryption) {
        return Err(KeyError::NotForSignatures);
    }
    if chosen
        .common
        .key_operations
        .as_ref()
        .is_some_and(|ops| !ops.contains(&KeyOperations::Verify))
    {
        return Err(KeyError::NotForVerification);
    }
    // The strongest statement a publisher can make about a key, and the one that
    // makes confusion a decode-time error rather than a guess. Compared through
    // `KeyAlgorithm` because that is the JWK-side type the `alg` member
    // deserialises into.
    if chosen
        .common
        .key_algorithm
        .is_some_and(|declared| declared != KeyAlgorithm::from(header.alg))
    {
        return Err(KeyError::AlgorithmMismatch);
    }
    // Deliberately no `Jwk::is_supported` gate here. It asks "does this build
    // implement the algorithm this key declares", which is only answerable when
    // the key declares one — and a key that declares none is exactly the
    // algorithm-confusion case, where it would refuse as `Unusable` before the
    // family check in `decode` ever ran. The two are redundant anyway: a key
    // whose type this build cannot handle is refused by `family_of`, and a key
    // of a type it can handle verifies or does not.
    Ok(chosen)
}

/// Builds a verification key from a chosen JWK.
///
/// The family check is a belt to `jsonwebtoken`'s braces: it refuses to verify
/// unless the provider's algorithm and the key's type agree, and stating it
/// here means the refusal is this crate's own and survives a backend swap.
/// The family is read from the JWK's own parameters — the part that came off
/// the wire — rather than inferred from a constructed key.
fn decode(jwk: &Jwk, header: &Header) -> Result<DecodingKey, KeyError> {
    if family_of(jwk)? != Family::of(header.alg) {
        return Err(KeyError::AlgorithmMismatch);
    }
    DecodingKey::from_jwk(jwk).map_err(|_| KeyError::Unusable)
}

/// Which side of the asymmetry a published key sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// `kty: oct` — a shared secret, and therefore HMAC.
    Secret,
    /// `RSA`, `EC` and `OKP`.
    Asymmetric,
}

impl Family {
    /// The family a named algorithm belongs to.
    const fn of(alg: Algorithm) -> Self {
        match alg {
            Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => Self::Secret,
            _ => Self::Asymmetric,
        }
    }
}

/// The family a JWK's key type puts it in.
const fn family_of(jwk: &Jwk) -> Result<Family, KeyError> {
    match &jwk.algorithm {
        AlgorithmParameters::OctetKey(_) => Ok(Family::Secret),
        AlgorithmParameters::RSA(_)
        | AlgorithmParameters::EllipticCurve(_)
        | AlgorithmParameters::OctetKeyPair(_) => Ok(Family::Asymmetric),
        // `AlgorithmParameters` is `#[non_exhaustive]`, and the arm it needs is
        // the one that matters: a `kty` this build has no key type for. Refused
        // rather than guessed, because an unknown key type is not a key we can
        // reason about being safe.
        _ => Err(KeyError::Unusable),
    }
}

#[cfg(test)]
impl KeySet {
    /// Installs a key set without fetching.
    ///
    /// Exists for the tests in `jwt::tests`, which are about what the source
    /// does with a key set rather than about how it obtains one. `fetched_at`
    /// is stamped so a test that seeds an old key and then expects a rotation
    /// refetch behaves the way it would in production.
    pub(super) fn seed(&self, keys: JwkSet) {
        self.cached.store(Arc::new(Cached {
            keys: Some(Arc::new(keys)),
            fetched_at: Some(Instant::now()),
        }));
    }
}
