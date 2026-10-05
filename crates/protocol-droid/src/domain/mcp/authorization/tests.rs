//! The authorization table, walked.
//!
//! These assert against the *expected* model per revision rather than against
//! whatever [`authorization_model`] returns, so a row that changes shape fails
//! here instead of quietly changing what the server and client do.

use super::{ClientRegistration, authorization_model};
use crate::domain::{ProtocolVersion, ReleaseDate};

/// Every revision this build has wire types for.
///
/// `KNOWN` rather than a literal list, so adding a revision to the enum fails
/// this file's own coverage test instead of silently inheriting the
/// conservative default that `authorization_model` hands an unknown revision.
const ALL: &[ProtocolVersion] = &ProtocolVersion::KNOWN;

#[test]
fn the_table_covers_every_revision() {
    // The table's per-revision assertions all iterate `ALL`, so this only
    // checks that `ALL` is the enum's own list rather than a copy that can fall
    // behind. What it cannot do is notice a *row* that stops matching; each of
    // those has its own assertion above, all of which walk `ALL`.
    assert!(ALL.contains(&ProtocolVersion::latest_known()));
    assert!(ALL.contains(&ProtocolVersion::earliest_known()));
}

#[test]
fn a_revision_published_after_this_build_gets_no_authorization_behaviour() {
    // `Unknown` is not "the newest known revision with newer numbers". It is a
    // revision whose spec this build has not read, and every row in the table is
    // a claim about prose somebody has to have read. So the default has to be
    // "nothing this build can justify" rather than the newest row, or an
    // unknown revision inherits `2026-07-28`'s requirements by accident.
    let unknown = ProtocolVersion::Unknown(ReleaseDate::V2026_07_28);
    let model = authorization_model(unknown);

    assert!(!model.specified);
    assert_eq!(model.client_registration, ClientRegistration::None);
    assert!(!model.protected_resource_metadata);
    assert!(!model.resource_indicators_required);
    assert!(!model.audience_bound());
}

#[test]
fn only_2024_11_05_lacks_an_authorization_framework() {
    for version in ALL {
        let expected = *version != ProtocolVersion::V2024_11_05;
        assert_eq!(
            authorization_model(*version).specified,
            expected,
            "{version} specified={expected} is wrong",
        );
    }
}

#[test]
fn protected_resource_metadata_arrives_in_2025_06_18() {
    // `2025-06-18` changelog §3 (PR #338). `2025-03-26` introduced OAuth 2.1 but
    // did not classify servers as resource servers, so it has nothing to
    // publish.
    for version in ALL {
        let expected = matches!(
            version,
            ProtocolVersion::V2025_06_18
                | ProtocolVersion::V2025_11_25
                | ProtocolVersion::V2026_07_28
        );
        assert_eq!(
            authorization_model(*version).protected_resource_metadata,
            expected,
            "{version} protected_resource_metadata={expected} is wrong",
        );
    }
}

#[test]
fn resource_indicators_are_required_from_2025_06_18() {
    // `2025-06-18` changelog §4 (PR #734).
    for version in ALL {
        let expected = matches!(
            version,
            ProtocolVersion::V2025_06_18
                | ProtocolVersion::V2025_11_25
                | ProtocolVersion::V2026_07_28
        );
        assert_eq!(
            authorization_model(*version).resource_indicators_required,
            expected,
            "{version} resource_indicators_required={expected} is wrong",
        );
    }
}

#[test]
fn oidc_discovery_arrives_in_2025_11_25() {
    // `2025-11-25` changelog §1 (PR #797).
    for version in ALL {
        let expected = matches!(
            version,
            ProtocolVersion::V2025_11_25 | ProtocolVersion::V2026_07_28
        );
        assert_eq!(
            authorization_model(*version).oidc_discovery,
            expected,
            "{version} oidc_discovery={expected} is wrong",
        );
    }
}

#[test]
fn incremental_scope_consent_arrives_in_2025_11_25() {
    // `2025-11-25` changelog §3 (SEP-835).
    for version in ALL {
        let expected = matches!(
            version,
            ProtocolVersion::V2025_11_25 | ProtocolVersion::V2026_07_28
        );
        assert_eq!(
            authorization_model(*version).incremental_scope_consent,
            expected,
            "{version} incremental_scope_consent={expected} is wrong",
        );
    }
}

#[test]
fn the_issuer_requirement_arrives_in_2026_07_28() {
    // `2026-07-28` changelog §7 (SEP-2468).
    for version in ALL {
        let expected = *version == ProtocolVersion::V2026_07_28;
        assert_eq!(
            authorization_model(*version).issuer_parameter,
            expected,
            "{version} issuer_parameter={expected} is wrong",
        );
    }
}

#[test]
fn credentials_become_issuer_bound_in_2026_07_28() {
    // `2026-07-28` changelog §9 (SEP-2352).
    for version in ALL {
        let expected = *version == ProtocolVersion::V2026_07_28;
        assert_eq!(
            authorization_model(*version).credentials_bound_to_issuer,
            expected,
            "{version} credentials_bound_to_issuer={expected} is wrong",
        );
    }
}

#[test]
fn registration_progresses_from_dynamic_to_metadata_documents() {
    // The one row where a wrong answer is a *deprecated* request rather than a
    // missing feature, so it gets its own per-revision assertion.
    let expected = [
        (ProtocolVersion::V2024_11_05, ClientRegistration::None),
        (ProtocolVersion::V2025_03_26, ClientRegistration::Dynamic),
        (ProtocolVersion::V2025_06_18, ClientRegistration::Dynamic),
        (
            ProtocolVersion::V2025_11_25,
            ClientRegistration::DynamicOrMetadataDocument,
        ),
        (
            ProtocolVersion::V2026_07_28,
            ClientRegistration::MetadataDocument,
        ),
    ];
    for (version, want) in expected {
        assert_eq!(
            authorization_model(version).client_registration,
            want,
            "{version} has the wrong registration mechanism",
        );
    }
}

#[test]
fn a_revision_with_no_framework_registers_with_nothing() {
    // `2024-11-05`. The client code walks `preferred()` before `fallback()`, so
    // a `None` here has to be total: there is no third step after both are
    // absent.
    let registration = authorization_model(ProtocolVersion::V2024_11_05).client_registration;
    assert_eq!(registration.preferred(), None);
    assert_eq!(registration.fallback(), None);
    assert!(!registration.supports_dynamic());
    assert!(!registration.supports_metadata_document());
}

#[test]
fn metadata_documents_are_preferred_wherever_they_exist() {
    // `2025-11-25` changelog §8 calls them *recommended*; `2026-07-28` §4
    // deprecates the alternative. Both point the same way, and a client that
    // leads with RFC 7591 against either is doing what its revision moved away
    // from.
    for version in [ProtocolVersion::V2025_11_25, ProtocolVersion::V2026_07_28] {
        assert_eq!(
            authorization_model(version).client_registration.preferred(),
            Some(ClientRegistration::MetadataDocument),
            "{version} should lead with a metadata document",
        );
    }
}

#[test]
fn dynamic_registration_is_the_fallback_not_the_removal() {
    // `2026-07-28` §4 keeps RFC 7591 "for backwards compatibility with
    // authorization servers that do not support Client ID Metadata Documents".
    // A client that refused outright would fail against exactly those servers.
    let latest = authorization_model(ProtocolVersion::V2026_07_28);
    assert!(latest.client_registration.supports_dynamic());
    assert_eq!(
        latest.client_registration.fallback(),
        Some(ClientRegistration::Dynamic)
    );
}

#[test]
fn the_walk_never_names_a_mechanism_the_revision_lacks() -> Result<(), TestError> {
    // Two steps, no state machine: `preferred` says what to try, `fallback` says
    // what to try if that was refused. Both have to name something the
    // revision actually supports, and a table row that got the mechanism
    // backwards would send a `2025-03-26` client to register with a client id
    // metadata document that revision has never heard of.
    for version in ALL {
        let registration = authorization_model(*version).client_registration;

        for (step, mechanism) in [
            ("preferred", registration.preferred()),
            ("fallback", registration.fallback()),
        ] {
            let Some(mechanism) = mechanism else {
                continue;
            };
            let supported = match mechanism {
                ClientRegistration::Dynamic => registration.supports_dynamic(),
                _ => registration.supports_metadata_document(),
            };
            if !supported {
                return Err(TestError::WalkNamesAnUnsupportedMechanism {
                    version: *version,
                    step,
                    mechanism,
                });
            }
        }
    }
    Ok(())
}

#[test]
fn a_fallback_is_only_offered_where_a_preference_was() {
    // `2025-03-26` has RFC 7591 and nothing else, so there is nothing to fall
    // back *to*. Offering a fallback there would send the client somewhere its
    // revision never defined.
    for version in ALL {
        let registration = authorization_model(*version).client_registration;
        if registration.preferred() == Some(ClientRegistration::Dynamic) {
            assert_eq!(
                registration.fallback(),
                None,
                "{version} has one mechanism but offers a second",
            );
        }
    }
}

#[test]
fn application_type_is_required_only_for_2026_07_28() {
    // `2026-07-28` changelog §8 (SEP-837).
    for version in ALL {
        let expected = *version == ProtocolVersion::V2026_07_28;
        assert_eq!(
            authorization_model(*version)
                .client_registration
                .application_type_required(),
            expected,
            "{version} application_type_required={expected} is wrong",
        );
    }
}

#[test]
fn audience_binding_follows_the_framework_not_the_metadata() {
    // `2025-03-26` has OAuth 2.1 but no protected-resource metadata, and a
    // token presented to it still has to be bound to this server. Gating this on
    // `protected_resource_metadata` would leave `2025-03-26` accepting any
    // token from any audience.
    for version in ALL {
        let model = authorization_model(*version);
        assert_eq!(
            model.audience_bound(),
            model.specified,
            "{version} audience_bound must follow specified",
        );
    }
    assert!(authorization_model(ProtocolVersion::V2025_03_26).audience_bound());
    assert!(!authorization_model(ProtocolVersion::V2024_11_05).audience_bound());
}

#[test]
fn the_derives_agree_and_declaration_order_is_chronological() {
    // `ClientRegistration` derives both `Ord` and `PartialOrd`. Since 1.98 a
    // derived `PartialOrd` takes a fast path that assumes a hand-written one
    // would agree with a derived `Ord` — here both are derived, so the claim
    // worth making is the weaker and more useful one: the variants are declared
    // in the order the revisions adopted them, so a derived `Ord` means
    // "less modern" and not something alphabetical.
    //
    // `None` first is deliberate: it is `2024-11-05`, the revision that had no
    // authorization framework at all.
    let chronological = [
        ClientRegistration::None,
        ClientRegistration::Dynamic,
        ClientRegistration::DynamicOrMetadataDocument,
        ClientRegistration::MetadataDocument,
    ];

    let mut sorted = chronological;
    sorted.sort();

    assert_eq!(
        sorted, chronological,
        "the declaration order no longer matches the derivation order",
    );

    // And the two derivations agree on every pair, which is what the 1.98 fast
    // path assumes when it skips `PartialOrd` and reads `Ord`.
    for a in sorted {
        for b in sorted {
            assert_eq!(
                Some(a.cmp(&b)),
                a.partial_cmp(&b),
                "{a:?} and {b:?} disagree between the two derivations",
            );
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error("{version} offers {mechanism:?} as its {step}, which it does not support")]
    WalkNamesAnUnsupportedMechanism {
        version: ProtocolVersion,
        step: &'static str,
        mechanism: ClientRegistration,
    },
}
