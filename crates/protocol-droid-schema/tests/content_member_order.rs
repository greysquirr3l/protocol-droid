//! Which arm of an untagged union a content block actually lands in.
//!
//! # The hazard
//!
//! The generated content unions are `#[serde(untagged)]`, which means serde
//! tries each member **in declaration order** and keeps the first that
//! round-trips. There is no discriminant in the JSON — a content block is
//! identified only by having the right shape — so the member index is doing the
//! work a `type` tag would normally do.
//!
//! That makes an ordering error invisible. `ImageContent` and `AudioContent`
//! are both `{ data: String, mimeType: String }`: a base64 string and a media
//! type. If image were tried first, an audio block would deserialize
//! *successfully* as an image, with no error anywhere, and the only symptom
//! would be a bridge serving a picture where a recording was sent.
//!
//! So this is worth testing on its own terms rather than assuming. The
//! generator emits the schema's own order, and the order is data, not an
//! implementation detail — but nothing else in the suite would notice if it
//! changed.
//!
//! # What is asserted
//!
//! For every revision that can carry a given content kind, a representative
//! block must deserialize into an arm that *re-serializes to the same JSON*.
//! Re-serialization is the discriminator: the arms differ in their `type`
//! value, so a block that landed in the wrong arm comes back with the wrong
//! tag and the comparison fails. A plain "does it deserialize" assertion would
//! pass for exactly the bug this is looking for.

// These tests assert on fallible parsing and panic with the failing fixture, so
// the expected lints are switched off for the file rather than every call site.
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on fallible parsing and panic with the failing fixture; propagating would replace a clear failure with noise"
)]

use protocol_droid_schema::wire::{
    draft, v2024_11_05, v2025_03_26, v2025_06_18, v2025_11_25, v2026_07_28,
};
use serde_json::{Value, json};

/// A block of `kind` that only that arm can accept.
fn block(kind: &str) -> Value {
    match kind {
        "text" => json!({"type": "text", "text": "hello"}),
        "image" => json!({"type": "image", "data": "AAA=", "mimeType": "image/png"}),
        // Structurally identical to image, which is the whole point.
        "audio" => json!({"type": "audio", "data": "AAA=", "mimeType": "audio/wav"}),
        "link" => json!({
            "type": "resource_link",
            "uri": "file:///a",
            "name": "a",
        }),
        "resource" => json!({
            "type": "resource",
            "resource": {"uri": "file:///a", "text": "body"},
        }),
        other => panic!("no fixture for {other}"),
    }
}

/// Deserializes a block and re-serializes it, so the arm it landed in is
/// visible in the result.
fn round_trip<T>(value: &Value) -> String
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let parsed: T = serde_json::from_value(value.clone()).expect("a known-good block");
    serde_json::to_string(&parsed).expect("the generated types serialise")
}

/// Asserts `kind` lands in an arm that re-serializes unchanged.
fn assert_arm_is_stable<T>(kind: &str)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let before = block(kind);
    let after: Value = serde_json::from_str(&round_trip::<T>(&before)).expect("re-parses");
    assert_eq!(
        after, before,
        "a {kind} block came back as a different arm: it landed somewhere that \
         serialises to a different shape, which is how an untagged union reports \
         a wrong member order"
    );
}

#[test]
fn the_2024_11_05_union_matches_its_content_kinds() {
    assert_arm_is_stable::<v2024_11_05::CallToolResultContentChoice>("text");
    assert_arm_is_stable::<v2024_11_05::CallToolResultContentChoice>("image");
    assert_arm_is_stable::<v2024_11_05::CallToolResultContentChoice>("resource");
}

#[test]
fn the_2025_03_26_union_matches_its_content_kinds() {
    assert_arm_is_stable::<v2025_03_26::CallToolResultContentChoice>("text");
    assert_arm_is_stable::<v2025_03_26::CallToolResultContentChoice>("image");
    assert_arm_is_stable::<v2025_03_26::CallToolResultContentChoice>("audio");
    assert_arm_is_stable::<v2025_03_26::CallToolResultContentChoice>("resource");
}

#[test]
fn the_2025_06_18_union_matches_its_content_kinds() {
    assert_arm_is_stable::<v2025_06_18::ContentBlock>("text");
    assert_arm_is_stable::<v2025_06_18::ContentBlock>("image");
    assert_arm_is_stable::<v2025_06_18::ContentBlock>("audio");
    assert_arm_is_stable::<v2025_06_18::ContentBlock>("link");
    assert_arm_is_stable::<v2025_06_18::ContentBlock>("resource");
}

#[test]
fn the_2025_11_25_union_matches_its_content_kinds() {
    assert_arm_is_stable::<v2025_11_25::ContentBlock>("text");
    assert_arm_is_stable::<v2025_11_25::ContentBlock>("image");
    assert_arm_is_stable::<v2025_11_25::ContentBlock>("audio");
    assert_arm_is_stable::<v2025_11_25::ContentBlock>("link");
    assert_arm_is_stable::<v2025_11_25::ContentBlock>("resource");
}

#[test]
fn the_2026_07_28_union_matches_its_content_kinds() {
    assert_arm_is_stable::<v2026_07_28::ContentBlock>("text");
    assert_arm_is_stable::<v2026_07_28::ContentBlock>("image");
    assert_arm_is_stable::<v2026_07_28::ContentBlock>("audio");
    assert_arm_is_stable::<v2026_07_28::ContentBlock>("link");
    assert_arm_is_stable::<v2026_07_28::ContentBlock>("resource");
}

#[test]
fn the_draft_union_matches_its_content_kinds() {
    assert_arm_is_stable::<draft::ContentBlock>("text");
    assert_arm_is_stable::<draft::ContentBlock>("image");
    assert_arm_is_stable::<draft::ContentBlock>("audio");
    assert_arm_is_stable::<draft::ContentBlock>("link");
    assert_arm_is_stable::<draft::ContentBlock>("resource");
}

/// Image and audio are the pair that can be confused, so they are checked
/// against each other rather than only for self-consistency: if a union ever
/// gained a second `data`/`mimeType` arm, one of these would land in the
/// other and the `type` tag on the way out would name the wrong kind.
#[test]
fn an_audio_block_is_not_accepted_as_an_image() {
    let audio = block("audio");
    let as_image: Result<v2025_06_18::ContentBlock, _> = serde_json::from_value(audio.clone());
    let image = block("image");
    let audio_as_image: Result<v2025_06_18::ContentBlock, _> =
        serde_json::from_value(image.clone());

    // Both parse, which is the hazard. What matters is that they come back out
    // as themselves.
    let audio_back: Value =
        serde_json::from_str(&round_trip::<v2025_06_18::ContentBlock>(&audio)).expect("parses");
    let image_back: Value =
        serde_json::from_str(&round_trip::<v2025_06_18::ContentBlock>(&image)).expect("parses");
    assert_eq!(audio_back, audio);
    assert_eq!(image_back, image);
    assert_ne!(audio_back, image_back);
    assert!(as_image.is_ok() || audio_as_image.is_ok());
}
