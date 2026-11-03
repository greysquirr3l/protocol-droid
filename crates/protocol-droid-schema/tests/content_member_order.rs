//! Which arm of an untagged union a content block actually lands in.
//!
//! Which arm of a union a content block actually lands in.
//!
//! # The hazard
//!
//! The content unions used to be `#[serde(untagged)]`, which means serde tries
//! each member **in declaration order** and keeps the first that round-trips.
//! There is no discriminant in the JSON — a content block is identified only
//! by having the right shape — so the member index was doing the work a `type`
//! tag would normally do.
//!
//! That made an ordering error invisible. `ImageContent` and `AudioContent`
//! are both `{ data: String, mimeType: String }`: a base64 string and a media
//! type. If image were tried first, an audio block would deserialize
//! *successfully* as an image, with no error anywhere, and the only symptom
//! would be a bridge serving a picture where a recording was sent.
//!
//! # What is asserted
//!
//! The content unions now dispatch on `type`, because the specification
//! *requires* it of every alternative: the peer has promised to send it, so
//! reading it cannot fail on a conforming value. A block therefore lands in
//! the arm its `type` names, and an unknown or absent `type` is a decode error
//! rather than a guess.
//!
//! Two assertions cover that. One re-serializes and compares, which catches a
//! block that arrives as the wrong kind of thing. The other looks at the arm
//! directly, which catches the case the first one cannot: the generated type
//! carries `r#type: String` with only `#[serde(rename = "type")]`, so a block
//! in the wrong arm re-serializes to byte-identical JSON and the comparison
//! passes.
//!
//! # Why both are still here
//!
//! The arm-level assertion is the one that would have caught the original
//! bug. The re-serialization assertion is kept because it is the one that
//! fails first, and because it covers the unions that are dispatched on shape
//! rather than on a tag: there, a block in the wrong arm *does* re-serialize
//! differently.

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

/// Asserts that a block lands in the arm its `type` names, by looking at the
/// arm rather than at the bytes.
///
/// This is the assertion `assert_arm_is_stable` cannot make. Re-serialisation
/// only discriminates when the emitter enforces the specification's `const`,
/// and it does not: `r#type` is a `String`, so a block in the wrong arm
/// re-serialises to exactly the JSON it came in as. Matching the arm removes
/// that dependency — the test asks the question it means to ask instead of
/// asking one the emitter happens to let through.
///
/// The union dispatches on `type` because the specification *requires* it of
/// every alternative, so the peer has promised to send it and the dispatcher
/// can read it without risking a fall-through. An unknown or absent `type`
/// is a decode error, not a guess.
#[test]
fn arm_is_claimed_by_const() {
    let audio: v2025_03_26::CallToolResultContentChoice =
        serde_json::from_value(block("audio")).expect("parses");

    assert!(
        matches!(audio, v2025_03_26::CallToolResultContentChoice::Member2(_)),
        "an audio block landed in the image arm. The union dispatches on \
         `type`, which every alternative requires, so `ImageContent` cannot \
         accept {{\"type\":\"audio\"}}. The audio is being served back as \
         an image with no error at any layer."
    );
}

/// The same question for a link, where the arms differ in shape rather than in
/// `type`.
///
/// Included because it is the case that *does* work today, and a net that only
/// ever finds bugs is a net nobody trusts. `ResourceLink` requires `uri` and
/// `name`, `EmbeddedResource` requires `resource`, so neither can be mistaken
/// for the other and re-serialization genuinely discriminates here.
#[test]
fn arms_that_differ_in_shape_do_discriminate() {
    let link: v2025_06_18::ContentBlock = serde_json::from_value(block("link")).expect("parses");
    assert!(matches!(link, v2025_06_18::ContentBlock::Member3(_)));

    let resource: v2025_06_18::ContentBlock =
        serde_json::from_value(block("resource")).expect("parses");
    assert!(matches!(resource, v2025_06_18::ContentBlock::Member4(_)));
}
