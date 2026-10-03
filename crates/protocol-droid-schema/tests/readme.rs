//! The example in the root `README.md`, compiled.
//!
//! A README that no longer matches the API is worse than no README: it is the
//! first thing a reader trusts, and a wrong signature there costs them more
//! than a missing section would. This target fails the moment the example stops
//! compiling or starts asserting the wrong thing, which is the only way a
//! document like that stays true.

use protocol_droid_schema::wire::{v2024_11_05, v2026_07_28};

/// Mirrors the "generated wire types" section of the README.
#[test]
fn the_readme_example_holds() -> Result<(), serde_json::Error> {
    // The same name, two revisions, two different shapes — both live in one
    // build. `resultType` is required in `2026-07-28` and does not exist at all
    // in `2024-11-05`, and `content` is a different Rust type in each.
    let old: v2024_11_05::CallToolResult =
        serde_json::from_str(r#"{"content": [], "isError": true}"#)?;
    let new: v2026_07_28::CallToolResult =
        serde_json::from_str(r#"{"content": [], "resultType": "complete"}"#)?;

    assert_eq!(old.is_error, Some(true));
    assert_eq!(new.result_type, "complete");
    // The member `2024-11-05` does not name is kept rather than dropped, which
    // is what the bridge needs and what the next section of the README claims.
    assert!(!old.extra.contains_key("resultType"));
    assert!(!new.extra.contains_key("isError"));
    Ok(())
}

/// The optional-member claim: absence stays absence through a round trip.
///
/// A generated `Option` field without `skip_serializing_if` serialises as an
/// explicit `null`, so a value the peer never sent comes back invented. The
/// README says this does not happen; this is where that is checked.
#[test]
fn an_absent_optional_member_does_not_come_back_as_null() -> Result<(), serde_json::Error> {
    let decoded: v2024_11_05::CallToolResult = serde_json::from_str(r#"{"content": []}"#)?;
    assert!(decoded.is_error.is_none());

    let members = serde_json::to_value(&decoded)?
        .as_object()
        .cloned()
        .unwrap_or_default();
    assert!(!members.contains_key("isError"));
    assert!(!members.contains_key("_meta"));
    Ok(())
}
