//! RFC 8707 resource indicators, bound to the grant they were requested for.
//!
//! # The attack this stops
//!
//! Without a resource indicator, an authorization server issues one token and
//! no record of which API it was for. A client that believes it is talking to
//! one server can be walked into presenting that token to another, and the
//! second server accepts it — because a bearer token names no audience, it
//! only names a bearer. RFC 8707 §1 puts it as a token confused-deputy problem:
//! the token is stolen from the resource it was meant for and replayed at one
//! it was not.
//!
//! The `2025-06-18` MCP changelog §4 (PR #734) requires the fix for exactly
//! this reason — clients **MUST** implement resource indicators "to prevent
//! malicious servers from obtaining access tokens" — so
//! [`AuthorizationModel::resource_indicators_required`](crate::domain::mcp::authorization::AuthorizationModel::resource_indicators_required)
//! is a client obligation, not an option.
//!
//! # Why the value is held across the redirect
//!
//! RFC 8707 §2.1 settles the code-flow question: where the authorization
//! endpoint returns an intermediate grant rather than a token, "the requested
//! resource is applicable to **the full authorization grant**". So the same
//! values have to appear in the token request that redeems the code.
//!
//! Sending `A` at authorization and `B` at redemption is the confused deputy,
//! written out: the authorization server sees two unrelated requests rather
//! than one grant, and may mint a token for `B` against a consent the user gave
//! for `A`. [`ResourceBinding`] holds what the grant was asked for and
//! answers whether a redemption still names it, so the two cannot drift.
//!
//! # Ordering
//!
//! RFC 8707 §2 says multiple `resource` parameters **MAY** be sent, and §5
//! notes that when several are the same audience order is not significant. So
//! the values are compared as a set: two requests naming the same resources in
//! a different order are the same request, and a duplicate is one value, not
//! two.

use core::fmt;

use crate::domain::mcp::authorization::AuthorizationModel;
use crate::domain::mcp::resource_metadata::ResourceUri;

/// The `resource` parameter values carried by one request.
///
/// Equality is set equality, not vector equality: RFC 8707 §5 says the order of
/// several `resource` parameters naming one audience is not significant, so a
/// redemption that lists the same resources in a different order names the same
/// grant. [`iter`](Self::iter) still yields request order, because that is what
/// gets serialised back out.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::mcp::resource_metadata::ResourceUri;
/// use protocol_droid::domain::oauth::ResourceParameters;
///
/// let api = ResourceUri::new("https://api.example.com/app/")?;
/// let parameters = ResourceParameters::single(&api);
/// assert_eq!(parameters.len(), 1);
/// // `ResourceUri` canonicalises, so the trailing slash is gone.
/// assert_eq!(parameters.iter().next(), Some("https://api.example.com/app"));
/// # Ok::<(), protocol_droid::domain::mcp::resource_metadata::ResourceUriError>(())
/// ```
#[derive(Clone, Default, Eq)]
pub struct ResourceParameters(Vec<ResourceUri>);

impl PartialEq for ResourceParameters {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.0.iter().all(|uri| other.0.contains(uri))
    }
}

impl ResourceParameters {
    /// The parameters for a request naming one resource — MCP's ordinary case,
    /// where a server has a single canonical identifier.
    #[must_use]
    pub fn single(resource: &ResourceUri) -> Self {
        Self(vec![resource.clone()])
    }

    /// The parameters for a request naming several, deduplicated with the first
    /// occurrence of each kept, so two spellings of the same set compare equal.
    #[must_use]
    pub fn many(resources: impl IntoIterator<Item = ResourceUri>) -> Self {
        let mut kept: Vec<ResourceUri> = Vec::new();
        for resource in resources {
            if !kept.contains(&resource) {
                kept.push(resource);
            }
        }
        Self(kept)
    }

    /// The values, in request order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(ResourceUri::as_str)
    }

    /// How many distinct resources are named.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no resource is named at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for ResourceParameters {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}

/// What an authorization grant was requested for, held until the code is
/// redeemed.
///
/// The client builds one from the values it sent in the authorization request
/// and consults it before sending the token request. It is deliberately not
/// `Copy` and has no accessor that hands back a mutable vector: the point is
/// that what the grant is bound to cannot be edited between the two requests.
#[derive(Clone, PartialEq, Eq)]
pub struct ResourceBinding {
    requested: ResourceParameters,
}

impl ResourceBinding {
    /// Bind a grant to the resources an authorization request named.
    ///
    /// # Errors
    ///
    /// [`ResourceIndicatorError::Missing`] when the revision requires resource
    /// indicators and the request named none. `2025-06-18` §4 makes this a
    /// **MUST** on the client, so a caller cannot get a binding for one of
    /// those revisions without naming a resource — the omission is not
    /// representable rather than being checked later and forgotten.
    pub fn bind(
        model: &AuthorizationModel,
        requested: ResourceParameters,
    ) -> Result<Self, ResourceIndicatorError> {
        if requested.is_empty() && model.resource_indicators_required {
            return Err(ResourceIndicatorError::Missing);
        }
        Ok(Self { requested })
    }

    /// The parameters the token request must carry.
    ///
    /// RFC 8707 §2.1: in the code flow the requested resource is applicable to
    /// the full grant, so these repeat at redemption rather than being left
    /// behind in the authorization request.
    #[must_use]
    pub const fn token_request(&self) -> &ResourceParameters {
        &self.requested
    }

    /// Whether a token request still names what the grant was bound to.
    ///
    /// `false` for a redemption that names a different resource, and also for
    /// one that names none: omitting the parameter is how a client silently
    /// asks for an unbounded token instead.
    #[must_use]
    pub fn matches_token_request(&self, offered: &ResourceParameters) -> bool {
        self.requested == *offered
    }
}

impl fmt::Debug for ResourceBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceBinding")
            .field("requested", &self.requested)
            .finish()
    }
}

/// An authorization request that does not carry what its revision requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ResourceIndicatorError {
    /// The revision requires a resource indicator and the request named none.
    ///
    /// `2025-06-18` changelog §4 (PR #734). Constructed rather than returned
    /// from a later stage so the omission cannot reach an authorization server.
    #[error(
        "this revision requires a resource indicator (RFC 8707), and the request named no resource"
    )]
    Missing,
}

#[cfg(test)]
mod tests {
    use super::{ResourceBinding, ResourceIndicatorError, ResourceParameters};
    use crate::domain::ProtocolVersion;
    use crate::domain::mcp::authorization::{AuthorizationModel, authorization_model};
    use crate::domain::mcp::resource_metadata::{ResourceUri, ResourceUriError};

    /// The fallible calls in this module answer two unrelated error types, so
    /// a test returns this rather than one `?` silently stopping at the first.
    #[derive(Debug)]
    enum Failure {
        Uri(ResourceUriError),
        Indicator(ResourceIndicatorError),
    }

    impl From<ResourceUriError> for Failure {
        fn from(error: ResourceUriError) -> Self {
            Self::Uri(error)
        }
    }

    impl From<ResourceIndicatorError> for Failure {
        fn from(error: ResourceIndicatorError) -> Self {
            Self::Indicator(error)
        }
    }

    /// Delegates so a failing test prints the engine's own account of what it
    /// refused, rather than `Failure::Uri(..)`. It also reads the fields, which
    /// is why they are not dead: nothing else in this module does.
    impl core::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Uri(error) => write!(formatter, "{error}"),
                Self::Indicator(error) => write!(formatter, "{error}"),
            }
        }
    }

    fn resource(value: &str) -> Result<ResourceUri, ResourceUriError> {
        ResourceUri::new(value)
    }

    fn model_for(version: ProtocolVersion) -> AuthorizationModel {
        authorization_model(version)
    }

    #[test]
    fn a_duplicated_resource_is_one_value() -> Result<(), Failure> {
        let api = resource("https://api.example.com/app/")?;
        let parameters = ResourceParameters::many([api.clone(), api]);
        assert_eq!(parameters.len(), 1);
        assert!(!parameters.is_empty());
        Ok(())
    }

    #[test]
    fn distinct_resources_are_kept_in_request_order() -> Result<(), Failure> {
        let first = resource("https://api.example.com/one/")?;
        let second = resource("https://api.example.com/two/")?;
        let parameters = ResourceParameters::many([first, second]);
        assert_eq!(parameters.len(), 2);
        // The fixtures carry a trailing slash and `ResourceUri` canonicalises
        // it away, so this pins what `iter` actually yields rather than what
        // was typed in.
        assert_eq!(
            parameters.iter().collect::<Vec<_>>(),
            vec!["https://api.example.com/one", "https://api.example.com/two"]
        );
        Ok(())
    }

    /// RFC 8707 §5: when several parameters name one audience, order is not
    /// significant. So a reordered request is the same request, not a mismatch.
    #[test]
    fn order_does_not_change_what_a_request_names() -> Result<(), Failure> {
        let one = resource("https://api.example.com/one")?;
        let two = resource("https://api.example.com/two")?;

        let binding = ResourceBinding::bind(
            &model_for(ProtocolVersion::V2025_06_18),
            ResourceParameters::many([one.clone(), two.clone()]),
        )?;

        let reordered = ResourceParameters::many([two, one]);
        assert!(
            binding.matches_token_request(&reordered),
            "a reordered set names the same resources"
        );
        Ok(())
    }

    /// The confused deputy, written out: `A` at authorization, `B` at
    /// redemption. If this ever returns true the binding is not doing its job.
    #[test]
    fn a_redemption_for_a_different_resource_does_not_match() -> Result<(), Failure> {
        let granted = resource("https://api.example.com/granted")?;
        let smuggled = resource("https://evil.example.com")?;

        let binding = ResourceBinding::bind(
            &model_for(ProtocolVersion::V2025_06_18),
            ResourceParameters::single(&granted),
        )?;

        assert!(
            !binding.matches_token_request(&ResourceParameters::single(&smuggled)),
            "a token request for another resource must not match the grant"
        );
        assert!(
            !binding.matches_token_request(&ResourceParameters::default()),
            "omitting the parameter must not match a bound grant"
        );
        assert!(binding.matches_token_request(&ResourceParameters::single(&granted)));
        Ok(())
    }

    /// `2025-06-18` §4: clients **MUST** implement resource indicators. The
    /// omission is unrepresentable for those revisions rather than caught by a
    /// caller remembering to check.
    #[test]
    fn a_revision_that_requires_indicators_refuses_an_empty_request() {
        for version in [
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ] {
            let model = model_for(version);
            assert!(
                model.resource_indicators_required,
                "{version:?} must require resource indicators"
            );
            assert_eq!(
                ResourceBinding::bind(&model, ResourceParameters::default()),
                Err(ResourceIndicatorError::Missing),
                "{version:?} must refuse an authorization request naming no resource"
            );
        }
    }

    /// Before `2025-06-18` there is no requirement, so an empty request binds.
    /// `2025-03-26` introduced OAuth 2.1 but the classification and the
    /// resource indicators both arrived a revision later (§3 and §4).
    #[test]
    fn a_revision_that_does_not_require_indicators_accepts_an_empty_request() {
        for version in [ProtocolVersion::V2024_11_05, ProtocolVersion::V2025_03_26] {
            let model = model_for(version);
            assert!(
                !model.resource_indicators_required,
                "{version:?} must not require resource indicators"
            );
            let binding = ResourceBinding::bind(&model, ResourceParameters::default());
            assert!(
                binding.is_ok(),
                "{version:?} must allow a request naming no resource"
            );
        }
    }

    /// The binding is what the token request repeats, so it has to survive a
    /// round trip through the parameters it was built from.
    #[test]
    fn the_binding_repeats_its_own_values_at_redemption() -> Result<(), Failure> {
        let api = resource("https://api.example.com/app")?;
        let requested = ResourceParameters::single(&api);
        let binding =
            ResourceBinding::bind(&model_for(ProtocolVersion::V2025_06_18), requested.clone())?;
        assert_eq!(binding.token_request(), &requested);
        assert!(binding.matches_token_request(&requested));
        Ok(())
    }
}
