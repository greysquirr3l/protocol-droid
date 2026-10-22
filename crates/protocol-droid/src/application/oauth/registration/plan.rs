//! The planning use case.
//!
//! Given an identity, the metadata it presents, and what the deployment can
//! reach, decide whether this client can be registered at all — and if not,
//! which of the two mechanisms is missing. The decision is made before any
//! network call, so a client that cannot be registered is told why before it
//! spends an attempt.

use super::ports::{Available, DynamicRegistrar, MetadataDocumentLoader};
use super::{ClientIdentity, RegistrarError, RegistrationError, RegistrationPlan};
use crate::domain::mcp::authorization::{AuthorizationModel, ClientRegistration};
use crate::domain::oauth::registration::{
    RegisteredClient, RegistrationDecision, decide_registration,
};

/// Decide how to register, without doing it.
///
/// The order is: reuse a stored credential the revision lets this client reuse,
/// then a published document, then dynamic registration. Document before
/// dynamic is not a preference — `2026-07-28` changelog §4 deprecates RFC 7591,
/// so leading with it against a server that supports both is a client
/// provoking a rejection.
///
/// # Errors
///
/// [`RegistrationError::IssuerBound`] if a stored credential belongs to another
/// issuer and the revision requires binding. [`RegistrationError::NoMechanism`]
/// if the revision names none. [`RegistrationError::NoUsableMechanism`] if what
/// the client has is a mechanism this revision does not define.
/// [`RegistrationError::ApplicationTypeRequired`] if dynamic registration is
/// planned for `2026-07-28` without `application_type` set.
pub fn plan_registration(
    model: &AuthorizationModel,
    issuer: &str,
    identity: &ClientIdentity,
    stored: Option<&RegisteredClient>,
    available: Available,
) -> Result<RegistrationPlan, RegistrationError> {
    if let Some(stored) = stored {
        match decide_registration(model, issuer, Some(stored)) {
            // Reused across issuers only where the revision does not require
            // binding, and even then the returned value carries its original
            // issuer, so nothing downstream can mistake it for this server's.
            RegistrationDecision::Reuse => {
                return Ok(RegistrationPlan::Reuse(stored.clone()));
            }
            RegistrationDecision::ReRegister => {
                return Err(RegistrationError::IssuerBound);
            }
            RegistrationDecision::Register => {}
        }
    }

    let mechanism = model.client_registration;
    if mechanism == ClientRegistration::None {
        return Err(RegistrationError::NoMechanism);
    }

    // A document the revision defines and this deployment can reach, with
    // dynamic registration as the fallback exactly when both of those are also
    // true and the client has metadata to fall back on.
    if let (true, Some(client_id)) = (
        mechanism.supports_metadata_document() && available.metadata_document(),
        identity.document(),
    ) {
        let fallback = identity
            .metadata()
            .filter(|_| mechanism.supports_dynamic() && available.dynamic())
            .cloned();
        return Ok(RegistrationPlan::FetchDocument {
            client_id: client_id.clone(),
            fallback,
        });
    }

    // Otherwise dynamic registration, if both the revision and the client allow
    // it. A document this revision cannot use is not silently ignored: if it is
    // the only thing the client has, there is nothing left to try.
    if let (true, Some(metadata)) = (mechanism.supports_dynamic(), identity.metadata()) {
        if mechanism.application_type_required() && metadata.application_type().is_none() {
            return Err(RegistrationError::ApplicationTypeRequired);
        }
        return Ok(RegistrationPlan::RegisterDynamic {
            metadata: metadata.clone(),
        });
    }

    Err(RegistrationError::NoUsableMechanism)
}

/// Plan a registration and carry it out.
///
/// Falls back from a document to dynamic registration when [`RegistrationPlan`]
/// carries a fallback and the document could not be used. It does **not** fall
/// back the other way: a client that publishes a document and cannot fetch it
/// has no identity to present, and registering one dynamically would create a
/// second identity rather than recover the first.
///
/// # Errors
///
/// Whatever [`plan_registration`] returns, or [`RegistrationError::DynamicFailed`]
/// if the fallback was needed and also failed.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::oauth::registration::{
///     ClientIdentity, ClientMetadata, acquire_client, plan_registration,
/// };
/// use protocol_droid::domain::ProtocolVersion;
/// use protocol_droid::domain::mcp::authorization::authorization_model;
/// use protocol_droid::domain::oauth::registration::ClientId;
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// use protocol_droid::application::oauth::registration::Available;
///
/// let model = authorization_model(ProtocolVersion::V2026_07_28);
/// let identity = ClientIdentity::from_document(
///     ClientId::metadata_url("https://client.example.com/metadata.json")?,
/// );
///
/// // Planning is pure: the document is preferred, and the plan says so.
/// let plan = plan_registration(
///     &model,
///     "https://auth.example.com",
///     &identity,
///     None,
///     Available::metadata_document_only(),
/// )?;
/// assert!(matches!(
///     plan,
///     protocol_droid::application::oauth::registration::RegistrationPlan::FetchDocument { .. }
/// ));
///
/// // Acting on it needs a loader to fetch with, so it is not exercised here.
/// # Ok(())
/// # }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub async fn acquire_client(
    model: &AuthorizationModel,
    issuer: &str,
    identity: &ClientIdentity,
    stored: Option<&RegisteredClient>,
    dynamic: Option<&(impl DynamicRegistrar + Sync)>,
    documents: Option<&(impl MetadataDocumentLoader + Sync)>,
) -> Result<RegisteredClient, RegistrationError> {
    let available = match (dynamic.is_some(), documents.is_some()) {
        (true, true) => Available::both(),
        (true, false) => Available::dynamic_only(),
        (false, true) => Available::metadata_document_only(),
        (false, false) => Available::neither(),
    };

    match plan_registration(model, issuer, identity, stored, available)? {
        RegistrationPlan::Reuse(client) => Ok(client),
        RegistrationPlan::FetchDocument {
            client_id,
            fallback,
        } => {
            let loader = documents.ok_or(RegistrationError::NoUsableMechanism)?;
            match loader.fetch_metadata_document(issuer, &client_id).await {
                Ok(client) => Ok(client),
                // Only a document that could not be *fetched* falls back. A
                // document that was fetched and is wrong is a forgery, and
                // falling back from it would let a client that has been
                // impersonated quietly register a fresh identity instead.
                Err(failure @ RegistrarError::Document(_)) => {
                    Err(RegistrationError::DocumentUnavailable(failure))
                }
                Err(failure) => match fallback {
                    Some(metadata) => {
                        let registrar = dynamic.ok_or(RegistrationError::NoUsableMechanism)?;
                        registrar
                            .register_dynamic(issuer, &metadata)
                            .await
                            .map_err(RegistrationError::DynamicFailed)
                    }
                    None => Err(RegistrationError::DocumentUnavailable(failure)),
                },
            }
        }
        RegistrationPlan::RegisterDynamic { metadata } => {
            let registrar = dynamic.ok_or(RegistrationError::NoUsableMechanism)?;
            registrar
                .register_dynamic(issuer, &metadata)
                .await
                .map_err(RegistrationError::DynamicFailed)
        }
    }
}
