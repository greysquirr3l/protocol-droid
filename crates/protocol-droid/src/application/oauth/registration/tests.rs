//! This module's tests.
//!
//! Out of the module so that reading `registration` means reading the types,
//! the ports, and the use case rather than the tests underneath all three.

use std::assert_matches;
use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::task::{Context, Poll, Waker};

use super::{
    Available, ClientIdentity, ClientMetadata, ClientMetadataError, DynamicRegistrar,
    MetadataDocumentLoader, RegistrarError, RegistrationError, RegistrationPlan, acquire_client,
    plan_registration,
};
use crate::domain::ProtocolVersion;
use crate::domain::mcp::authorization::{AuthorizationModel, authorization_model};
use crate::domain::oauth::registration::{ClientId, ClientIdError, RegisteredClient};

const AS: &str = "https://auth.example.com";
const OTHER_AS: &str = "https://other-auth.example.com";
const URL: &str = "https://client.example.com/metadata.json";

/// The revisions whose authorization spec differs in a way this module
/// acts on. A bare `AuthorizationModel` would let a test pick a revision by
/// accident and assert something true of all of them.
const EARLY: ProtocolVersion = ProtocolVersion::V2024_11_05;
const DYNAMIC_ONLY: ProtocolVersion = ProtocolVersion::V2025_03_26;
const BOTH: ProtocolVersion = ProtocolVersion::V2025_11_25;
const DOCUMENT_FIRST: ProtocolVersion = ProtocolVersion::V2026_07_28;

fn model(version: ProtocolVersion) -> AuthorizationModel {
    authorization_model(version)
}

/// The error types these tests answer, collected so `?` works and a failure
/// prints the engine's own account of what it refused.
#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error(transparent)]
    Id(#[from] ClientIdError),
    #[error(transparent)]
    Metadata(#[from] ClientMetadataError),
    #[error(transparent)]
    Registration(#[from] RegistrationError),
    #[error("a fake port pended, which none of them should")]
    Pended,
}

impl From<RegistrarError> for Failure {
    fn from(error: RegistrarError) -> Self {
        Self::Registration(RegistrationError::DynamicFailed(error))
    }
}

/// Drives a future that is expected never to pend, and says so when it
/// does.
///
/// `#[tokio::test]` is not available here: this module is gated on `oauth`,
/// and the tokio macros come in with `http`. Pulling a runtime in to test
/// fakes that return immediately would be a heavier dependency than the
/// thing under test. `Waker::noop` is correct for a future that resolves on
/// its first poll, and returning `None` on `Pending` is what makes that
/// assumption checkable rather than merely stated.
fn now<T>(future: impl Future<Output = T>) -> Option<T> {
    let mut future = std::pin::pin!(future);
    let waker = Waker::noop();
    match future.as_mut().poll(&mut Context::from_waker(waker)) {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}

fn metadata() -> Result<ClientMetadata, Failure> {
    Ok(
        ClientMetadata::new(vec!["https://client.example.com/callback".to_owned()])?
            .with_client_name("Example"),
    )
}

fn document_identity() -> Result<ClientIdentity, Failure> {
    Ok(ClientIdentity::from_document(ClientId::metadata_url(URL)?))
}

/// A dynamic registrar that always succeeds, counting its calls so a test
/// can prove it was *not* reached.
struct Dynamic {
    calls: AtomicU32,
}

impl DynamicRegistrar for Dynamic {
    // Not `async`: this fake resolves on its first poll, so `async` here
    // would be an async block with no await point in it.
    fn register_dynamic(
        &self,
        issuer: &str,
        _metadata: &ClientMetadata,
    ) -> impl Future<Output = Result<RegisteredClient, RegistrarError>> + Send {
        self.calls.fetch_add(1, Ordering::Relaxed);
        std::future::ready(
            RegisteredClient::dynamic(issuer, ClientId::opaque("dynamic-1"), None)
                .map_err(Into::into),
        )
    }
}

/// A document loader that answers with whatever it was built to answer.
struct Loader {
    outcome: Result<(), RegistrarError>,
    calls: AtomicU32,
}

impl Loader {
    fn serving() -> Self {
        Self {
            outcome: Ok(()),
            calls: AtomicU32::new(0),
        }
    }

    fn failing(error: RegistrarError) -> Self {
        Self {
            outcome: Err(error),
            calls: AtomicU32::new(0),
        }
    }
}

impl MetadataDocumentLoader for Loader {
    fn fetch_metadata_document(
        &self,
        issuer: &str,
        client_id: &ClientId,
    ) -> impl Future<Output = Result<RegisteredClient, RegistrarError>> + Send {
        self.calls.fetch_add(1, Ordering::Relaxed);
        std::future::ready(match &self.outcome {
            Ok(()) => Ok(RegisteredClient::from_metadata_document(
                issuer,
                client_id.clone(),
            )),
            Err(failure) => Err(failure.clone()),
        })
    }
}

// --- what each revision allows -----------------------------------------

/// `2024-11-05` has no authorization entry at all, so a client that
/// registers here is doing something the revision never asked for. Proceeding
/// quietly is how that goes unnoticed.
#[test]
fn a_revision_with_no_authorization_framework_refuses_to_register() -> Result<(), Failure> {
    assert_matches!(
        plan_registration(
            &model(EARLY),
            AS,
            &ClientIdentity::from_metadata(metadata()?),
            None,
            Available::both(),
        ),
        Err(RegistrationError::NoMechanism)
    );
    Ok(())
}

/// `2025-03-26` names RFC 7591 and nothing else. A client holding only a
/// document has nothing it can use, and saying so beats silently
/// registering a second identity.
#[test]
fn a_document_is_not_usable_where_the_revision_never_named_one() -> Result<(), Failure> {
    assert_matches!(
        plan_registration(
            &model(DYNAMIC_ONLY),
            AS,
            &document_identity()?,
            None,
            Available::both(),
        ),
        Err(RegistrationError::NoUsableMechanism)
    );
    Ok(())
}

#[test]
fn a_revision_naming_only_dynamic_registers_dynamically() -> Result<(), Failure> {
    let plan = plan_registration(
        &model(DYNAMIC_ONLY),
        AS,
        &ClientIdentity::from_metadata(metadata()?),
        None,
        Available::dynamic_only(),
    )?;

    assert_matches!(plan, RegistrationPlan::RegisterDynamic { .. });
    Ok(())
}

/// The ordering the whole module exists for. `2026-07-28` changelog §4
/// deprecates RFC 7591 "in favor of" documents, so a client with both must
/// lead with the document — a client that leads with the deprecated
/// mechanism against a server supporting both is provoking a rejection.
#[test]
fn a_document_is_preferred_even_when_the_client_can_also_register() -> Result<(), Failure> {
    let identity = document_identity()?.with_metadata_fallback(metadata()?);

    let plan = plan_registration(
        &model(DOCUMENT_FIRST),
        AS,
        &identity,
        None,
        Available::both(),
    )?;

    assert_matches!(plan, RegistrationPlan::FetchDocument { .. });
    Ok(())
}

/// `2025-11-25` changelog §8 recommends documents without deprecating RFC
/// 7591, so the same ordering applies a revision earlier.
#[test]
fn the_preference_predates_the_deprecation() -> Result<(), Failure> {
    let identity = document_identity()?.with_metadata_fallback(metadata()?);

    let plan = plan_registration(&model(BOTH), AS, &identity, None, Available::both())?;

    assert_matches!(plan, RegistrationPlan::FetchDocument { .. });
    Ok(())
}

/// A deployment that wired no document loader must not be handed a plan to
/// fetch one.
#[test]
fn a_document_needs_a_loader_to_fetch_it_with() -> Result<(), Failure> {
    let identity = document_identity()?;

    assert_matches!(
        plan_registration(
            &model(DOCUMENT_FIRST),
            AS,
            &identity,
            None,
            Available::dynamic_only(),
        ),
        Err(RegistrationError::NoUsableMechanism)
    );
    Ok(())
}

// --- the MUST that has to be caught before anything is sent ------------

/// `2026-07-28` §8 makes `application_type` a MUST for dynamic
/// registration. Catching it at plan time is the difference between a clear
/// refusal and a registered client the server will not honour.
#[test]
fn the_required_application_type_is_refused_before_anything_is_sent() -> Result<(), Failure> {
    assert_matches!(
        plan_registration(
            &model(DOCUMENT_FIRST),
            AS,
            &ClientIdentity::from_metadata(metadata()?),
            None,
            Available::dynamic_only(),
        ),
        Err(RegistrationError::ApplicationTypeRequired)
    );
    Ok(())
}

#[test]
fn supplying_the_application_type_satisfies_the_requirement() -> Result<(), Failure> {
    let identity = ClientIdentity::from_metadata(metadata()?.with_application_type("native"));

    assert_matches!(
        plan_registration(
            &model(DOCUMENT_FIRST),
            AS,
            &identity,
            None,
            Available::dynamic_only(),
        ),
        Ok(RegistrationPlan::RegisterDynamic { .. })
    );
    Ok(())
}

/// `2026-07-28` deprecates RFC 7591 rather than removing it, so a server
/// implementing only RFC 7591 is not misconfigured and a client must still
/// be able to use it.
#[test]
fn the_deprecated_mechanism_is_still_available() {
    assert!(
        model(DOCUMENT_FIRST).client_registration.supports_dynamic(),
        "2026-07-28 still permits RFC 7591"
    );
}

// --- stored credentials ------------------------------------------------

#[test]
fn a_stored_credential_from_this_issuer_is_reused() -> Result<(), Failure> {
    let stored = RegisteredClient::dynamic(AS, ClientId::opaque("stored"), None)?;

    let plan = plan_registration(
        &model(DOCUMENT_FIRST),
        AS,
        &document_identity()?,
        Some(&stored),
        Available::both(),
    )?;

    assert_matches!(plan, RegistrationPlan::Reuse(ref client) if client == &stored);
    Ok(())
}

/// `2026-07-28` binds a credential to its issuer so a credential minted by
/// one server cannot be presented to another. A stored credential from
/// elsewhere has to be refused, not quietly reused.
#[test]
fn a_credential_from_another_issuer_is_refused_where_binding_is_required() -> Result<(), Failure> {
    let stored = RegisteredClient::dynamic(OTHER_AS, ClientId::opaque("stored"), None)?;

    assert_matches!(
        plan_registration(
            &model(DOCUMENT_FIRST),
            AS,
            &document_identity()?,
            Some(&stored),
            Available::both(),
        ),
        Err(RegistrationError::IssuerBound)
    );
    Ok(())
}

/// Older revisions do not require binding, so the same stored credential
/// is reused. It still carries the issuer it was issued by, so nothing
/// downstream can mistake it for the current server's.
#[test]
fn a_credential_from_another_issuer_is_reused_where_binding_is_not_required() -> Result<(), Failure>
{
    let stored = RegisteredClient::dynamic(OTHER_AS, ClientId::opaque("stored"), None)?;

    let plan = plan_registration(
        &model(DYNAMIC_ONLY),
        AS,
        &ClientIdentity::from_metadata(metadata()?),
        Some(&stored),
        Available::dynamic_only(),
    )?;

    assert_matches!(plan, RegistrationPlan::Reuse(ref client) if client.is_issued_by(OTHER_AS));
    Ok(())
}

// --- executing the plan ------------------------------------------------

#[test]
fn a_served_document_is_used_without_registering() -> Result<(), Failure> {
    let dynamic = Dynamic {
        calls: AtomicU32::new(0),
    };
    let loader = Loader::serving();

    let client = match now(acquire_client(
        &model(DOCUMENT_FIRST),
        AS,
        &document_identity()?,
        None,
        Some(&dynamic),
        Some(&loader),
    )) {
        Some(result) => result?,
        None => return Err(Failure::Pended),
    };

    assert_eq!(client.client_id().as_str(), URL);
    assert_eq!(loader.calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        dynamic.calls.load(Ordering::Relaxed),
        0,
        "the document path registered anyway"
    );
    Ok(())
}

/// `2026-07-28` keeps RFC 7591 for servers that do not implement documents,
/// and a document that does not resolve is exactly that server.
#[test]
fn an_unreachable_document_falls_back_to_dynamic_registration() -> Result<(), Failure> {
    let dynamic = Dynamic {
        calls: AtomicU32::new(0),
    };
    let loader = Loader::failing(RegistrarError::Unavailable("connection refused".into()));
    let identity = document_identity()?.with_metadata_fallback(metadata()?);

    let client = match now(acquire_client(
        &model(DOCUMENT_FIRST),
        AS,
        &identity,
        None,
        Some(&dynamic),
        Some(&loader),
    )) {
        Some(result) => result?,
        None => return Err(Failure::Pended),
    };

    assert_eq!(client.client_id().as_str(), "dynamic-1");
    assert_eq!(dynamic.calls.load(Ordering::Relaxed), 1);
    Ok(())
}

/// The distinction that matters. A document that could not be *fetched* is a
/// server without documents; a document that was fetched and does not claim
/// the URL it came from is a forgery, and falling back from it would let an
/// impersonated client quietly register a fresh identity instead of
/// surfacing the problem.
#[test]
fn a_document_that_is_present_but_wrong_never_falls_back() -> Result<(), Failure> {
    let dynamic = Dynamic {
        calls: AtomicU32::new(0),
    };
    let loader = Loader::failing(RegistrarError::Document(ClientIdError::DocumentMismatch));
    let identity = document_identity()?.with_metadata_fallback(metadata()?);

    let Some(outcome) = now(acquire_client(
        &model(DOCUMENT_FIRST),
        AS,
        &identity,
        None,
        Some(&dynamic),
        Some(&loader),
    )) else {
        return Err(Failure::Pended);
    };

    assert_matches!(outcome, Err(RegistrationError::DocumentUnavailable(_)));
    assert_eq!(
        dynamic.calls.load(Ordering::Relaxed),
        0,
        "a forged document silently became a fresh registration"
    );
    Ok(())
}

/// With no metadata to fall back on, an unreachable document is just a
/// failure.
#[test]
fn an_unreachable_document_with_no_fallback_is_an_error() -> Result<(), Failure> {
    let dynamic = Dynamic {
        calls: AtomicU32::new(0),
    };
    let loader = Loader::failing(RegistrarError::Unavailable("connection refused".into()));

    let Some(outcome) = now(acquire_client(
        &model(DOCUMENT_FIRST),
        AS,
        &document_identity()?,
        None,
        Some(&dynamic),
        Some(&loader),
    )) else {
        return Err(Failure::Pended);
    };

    assert_matches!(outcome, Err(RegistrationError::DocumentUnavailable(_)));
    assert_eq!(dynamic.calls.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
fn a_stored_credential_is_returned_without_any_port_being_called() -> Result<(), Failure> {
    let dynamic = Dynamic {
        calls: AtomicU32::new(0),
    };
    let loader = Loader::serving();
    let stored = RegisteredClient::dynamic(AS, ClientId::opaque("stored"), None)?;

    let client = match now(acquire_client(
        &model(DOCUMENT_FIRST),
        AS,
        &document_identity()?,
        Some(&stored),
        Some(&dynamic),
        Some(&loader),
    )) {
        Some(result) => result?,
        None => return Err(Failure::Pended),
    };

    assert_eq!(&client, &stored);
    assert_eq!(loader.calls.load(Ordering::Relaxed), 0);
    assert_eq!(dynamic.calls.load(Ordering::Relaxed), 0);
    Ok(())
}

// --- the metadata itself ----------------------------------------------

/// RFC 7591 §2 makes `redirect_uris` the one required member.
#[test]
fn metadata_without_redirect_uris_is_refused() {
    assert!(ClientMetadata::new(Vec::new()).is_err());
}

/// A bare path is what a half-filled configuration produces, and a server
/// would reject it.
#[test]
fn metadata_with_a_relative_redirect_uri_is_refused() {
    assert!(ClientMetadata::new(vec!["/callback".to_owned()]).is_err());
}

#[test]
fn a_loopback_redirect_is_allowed() -> Result<(), Failure> {
    // RFC 8252 §7.3 makes the native-app loopback redirect legal, so an
    // https-only rule would refuse a legitimate client.
    let metadata = ClientMetadata::new(vec!["http://127.0.0.1:8080/callback".to_owned()])?;

    assert_eq!(metadata.redirect_uris().len(), 1);
    Ok(())
}

/// The named fields are the ones this crate gates on, so the passthrough
/// must not be able to set one — otherwise a document could claim redirect
/// URIs no constructor ever checked.
#[test]
fn the_passthrough_cannot_overwrite_a_modelled_field() -> Result<(), Failure> {
    let metadata = metadata()?;

    assert_matches!(
        metadata.with_field("redirect_uris", serde_json::json!(["https://evil.example.com"])),
        Err(ClientMetadataError::ReservedField(ref name)) if name == "redirect_uris"
    );
    Ok(())
}

/// `2025-11-25` and later name `client_uri`, `icon_uri` and `title`. A
/// client that publishes one and has it dropped at a struct boundary has
/// registered something other than what it meant to.
#[test]
fn an_unmodelled_field_reaches_the_wire() -> Result<(), Failure> {
    let metadata = metadata()?
        .with_field("client_uri", "https://client.example.com")?
        .with_field("title", "Example Client")?;

    let document = serde_json::to_value(&metadata)
        .map_err(|_| Failure::Registration(RegistrationError::NoMechanism))?;

    assert_eq!(
        document.get("client_uri"),
        Some(&serde_json::json!("https://client.example.com"))
    );
    assert_eq!(
        document.get("title"),
        Some(&serde_json::json!("Example Client"))
    );
    Ok(())
}

/// The named fields and the passthrough coexist on the wire without either
/// shadowing the other.
#[test]
fn modelled_and_extra_fields_coexist() -> Result<(), Failure> {
    let metadata = metadata()?
        .with_application_type("native")
        .with_field("title", "Example")?;

    let document = serde_json::to_value(&metadata)
        .map_err(|_| Failure::Registration(RegistrationError::NoMechanism))?;

    assert_eq!(
        document.get("application_type"),
        Some(&serde_json::json!("native"))
    );
    assert_eq!(
        document.get("client_name"),
        Some(&serde_json::json!("Example"))
    );
    assert_eq!(document.get("title"), Some(&serde_json::json!("Example")));
    assert_eq!(
        document.get("redirect_uris"),
        Some(&serde_json::json!(["https://client.example.com/callback"]))
    );
    Ok(())
}
