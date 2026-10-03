//! Property-based proof that the generated types accept and re-emit the wire
//! format without loss.
//!
//! # What is actually being asserted
//!
//! For an arbitrary value `V` drawn to satisfy a definition `D` of a revision's
//! `schema.json`:
//!
//! 1. `V` validates against `D` — checked with `jsonschema`, so the generator in
//!    [`schema_value`] is held to the specification rather than to itself.
//! 2. `V` decodes into the generated Rust type for `D`.
//! 3. Re-encoding produces a value that still validates against `D`.
//! 4. That value equals `V`.
//!
//! Step 4 is the one that matters and the one the fixed fixtures in
//! `roundtrip.rs` cannot reach: it is the claim the whole crate exists for. A
//! member that decodes into a field and comes back under a different name, a
//! `null` where the peer sent nothing, or an untagged union that picks a
//! different branch on the way out, is the `version-translating` bridge
//! silently dropping data — and those are exactly the bugs a human does not
//! think to write a fixture for.
//!
//! Step 3 is separate from step 4 on purpose. A type can round-trip a value
//! perfectly and still emit something the peer would reject, such as a required
//! member that was consumed and never re-emitted.
//!
//! # Why each case names its Rust type
//!
//! One test sampling a case index and looking the type up dynamically could not
//! exist — Rust has no runtime type registry — so the mapping from definition to
//! type is written out. That is the right shape: writing it out is what makes
//! this a compile-checked assertion that the definition `CallToolResult` really
//! does produce `wire::v2024_11_05::CallToolResult`.

// The module lives in `tests/property/` rather than beside this file so the
// directory holds only the one supporting concern for this target.
//
// `pub`, not private: a private module makes its `pub` items unreachable, which
// `unreachable_pub` flags, while narrowing them to `pub(crate)` inside a
// private module is what `redundant_pub_crate` flags. Declaring the module
// reachable is the only visibility both lints accept, and in a test binary
// nothing outside the crate can see it either way.
#[path = "property/schema_value.rs"]
pub mod schema_value;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use proptest::prelude::*;
use serde_json::Value;

use schema_value::{Defs, defs_of, value_for_def};

/// How many cases each definition is tried with.
///
/// A definition with a union of `n` alternatives and `m` optional members has
/// more than `n·m` distinguishable shapes, and the fixed fixtures in
/// `roundtrip.rs` only cover the hand-picked ones. 256 is the point where the
/// whole suite still finishes in about a second, so the budget goes on coverage
/// rather than on shaving time off a test.
const CASES: u32 = 256;

/// The `spec` directory, located by walking up from the crate manifest.
fn spec_dir() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<workspace>/crates/<crate>` by construction, so
    // walking up two levels is arithmetic rather than a guess.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map_or_else(PathBuf::new, |root| {
            root.join("crates/protocol-droid-schema/spec")
        })
}

/// The parsed `schema.json` for one revision.
///
/// An unreadable or malformed file yields `null`, which every caller then treats
/// as "no definitions" — so a missing fixture surfaces as a clear "no
/// definition named X" rather than a panic from a helper.
fn schema_for(revision: &str) -> Value {
    let path = spec_dir().join(revision).join("schema.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// The definitions of one revision, keyed by name.
fn defs_for(revision: &str) -> Defs {
    defs_of(&schema_for(revision))
}

/// A strategy producing values `definition` in `revision` accepts.
///
/// Owned rather than borrowing the definitions, so the strategy expression in
/// each `proptest!` block has no temporary whose lifetime the expansion would
/// have to get right.
fn values(revision: &str, definition: &str) -> BoxedStrategy<Value> {
    let defs = defs_for(revision);
    value_for_def(definition, &defs).unwrap_or_else(|| {
        // Reached only when a case names a definition the vendored schema does
        // not have, which is a stale test list rather than a generator fault.
        // Said here so the failure does not read as the latter.
        fail(&format!(
            "{revision}: the case list names a definition the vendored \
             schema does not have ({definition}); grep spec/{revision}/schema.json \
             before adding a name"
        ))
    })
}

/// Compiled validators, keyed by `revision/definition`.
///
/// `jsonschema` compiles a schema document on every call, and each property
/// case asks for the same one sixty times. The cache is process-wide because a
/// test binary is single-process and the alternative is a recompile per case.
static VALIDATORS: LazyLock<Mutex<HashMap<String, jsonschema::Validator>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A validator for the definition called `definition` in `revision`.
///
/// The whole definitions map is embedded so a `$ref` *inside* the definition
/// still resolves, and the document root is a `$ref` to the definition itself.
/// The container keyword is read from the file rather than assumed: the first
/// three revisions are draft-07 and say `definitions`, the later ones are
/// 2020-12 and say `$defs`, and guessing wrong makes every reference dangle.
fn validator_for(revision: &str, definition: &str) -> jsonschema::Validator {
    let key = format!("{revision}/{definition}");
    if let Some(cached) = VALIDATORS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return cached.clone();
    }

    let schema = schema_for(revision);
    let container = if schema.get("$defs").is_some() {
        "$defs"
    } else {
        "definitions"
    };
    if !defs_for(revision).contains_key(definition) {
        fail(&format!("{revision}: no definition named {definition}"));
    }

    // The *whole* document, with a `$ref` to the definition added at the root.
    // Extracting just the one definition does not work: `AudioContent` refers
    // to `Annotations`, which refers to `Role` and `ContentBlock`, and a
    // document containing only the target has nothing for those to resolve
    // against. The root of every vendored file carries nothing but `$schema`,
    // so adding a sibling `$ref` changes nothing else.
    let mut document = schema;
    let Some(root) = document.as_object_mut() else {
        fail(&format!(
            "{revision}: the vendored schema is not a JSON object"
        ));
    };
    root.insert(
        "$ref".to_owned(),
        Value::String(format!("#/{container}/{definition}")),
    );

    let compiled = jsonschema::validator_for(&document)
        .unwrap_or_else(|err| fail(&format!("{key}: the document will not compile: {err}")));

    VALIDATORS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, compiled.clone());
    compiled
}

/// Abort the test with `message`.
///
/// `std::process::exit` is the only divergence available once `panic!` and
/// `unreachable!` are denied workspace-wide. The non-zero status is what the
/// harness reads, and the message goes to stderr so the failure stays legible.
fn fail(message: &str) -> ! {
    eprintln!("property test failed: {message}");
    std::process::exit(1)
}

/// Assert the four steps above for one case.
#[track_caller]
fn assert_round_trips<T>(revision: &str, definition: &str, value: &Value)
where
    T: for<'de> serde::Deserialize<'de> + serde::Serialize,
{
    let validator = validator_for(revision, definition);

    // 1. The generator's own output must satisfy the specification. A failure
    //    here is the value generator's bug, and the message says which, because
    //    the two failure modes are easy to confuse.
    if let Err(errors) = validator.validate(value) {
        fail(&format!(
            "{revision}/{definition}: the value generator produced something \
             the schema rejects\n  value: {value:#}\n  error: {errors:#}"
        ));
    }

    // 2. The generated type must accept it.
    let Ok(decoded) = serde_json::from_value::<T>(value.clone()) else {
        let reason = decode_error::<T>(value);
        fail(&format!(
            "{revision}/{definition}: a schema-valid value failed to decode: \
             {reason}\n  value: {value:#}"
        ));
    };

    let Ok(re_encoded) = serde_json::to_value(&decoded) else {
        let reason = serde_json::to_value(&decoded)
            .err()
            .map_or_else(|| "unknown".to_owned(), |err| err.to_string());
        fail(&format!(
            "{revision}/{definition}: a decoded value failed to encode: {reason}"
        ));
    };

    // 3. What comes back must still satisfy the specification. A type that
    //    round-trips a value and then emits something the peer would reject is
    //    still a wire-format bug.
    if let Err(errors) = validator.validate(&re_encoded) {
        fail(&format!(
            "{revision}/{definition}: re-encoding produced a value the schema \
             rejects\n  before: {value:#}\n  after:  {re_encoded:#}\n  \
             error:  {errors:#}"
        ));
    }

    // 4. And it must be the same value. Compared as `Value`, not as text, so
    //    member ordering — which JSON does not distinguish — is not a failure.
    if re_encoded != *value {
        fail(&format!(
            "{revision}/{definition}: the round trip changed the value\n  \
             before: {value:#}\n  after:  {re_encoded:#}"
        ));
    }
}

/// The reason `value` did not decode as `T`, as a string.
///
/// Decoding twice to render the error is deliberate: `unwrap_or_else` would have
/// to produce a `T` to report on, and there is no `T` to produce. The failure
/// path is not hot.
fn decode_error<T: for<'de> serde::Deserialize<'de>>(value: &Value) -> String {
    serde_json::from_value::<T>(value.clone())
        .err()
        .map_or_else(|| "unknown".to_owned(), |err| err.to_string())
}

/// One property test per `(definition, generated type)` pair.
///
/// `$revision` and `$definition` locate the case; `$ty` is the generated Rust
/// type the value has to survive a round trip through, and naming it here is
/// what makes the mapping from schema to type a compile-checked fact.
macro_rules! round_trips {
    ($($revision:literal / $definition:literal as $test:ident : $ty:ty),* $(,)?) => {$(
        proptest! {
            // Failure persistence off. It writes a regression file next to the
            // source on failure, and it needs a `lib.rs` or `main.rs` to find —
            // an integration test target is named after the file, so every case
            // printed a warning to stderr before running a single sample.
            #![proptest_config(proptest::test_runner::Config {
                cases: CASES,
                failure_persistence: None,
                ..proptest::test_runner::Config::default()
            })]
            #[test]
            fn $test(value in values($revision, $definition)) {
                assert_round_trips::<$ty>($revision, $definition, &value);
            }
        }
    )*};
}

round_trips! {
    // 2024-11-05. The content types are separate definitions here rather than a
    // `ContentBlock` union, so the union path is exercised on 2025-06-18 and
    // later.
    "2024-11-05" / "JSONRPCMessage" as jsonrpc_message_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::JSONRPCMessage,
    "2024-11-05" / "JSONRPCRequest" as jsonrpc_request_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::JSONRPCRequest,
    "2024-11-05" / "JSONRPCNotification" as jsonrpc_notification_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::JSONRPCNotification,
    "2024-11-05" / "JSONRPCResponse" as jsonrpc_response_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::JSONRPCResponse,
    "2024-11-05" / "JSONRPCError" as jsonrpc_error_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::JSONRPCError,
    "2024-11-05" / "CallToolRequest" as call_tool_request_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::CallToolRequest,
    "2024-11-05" / "CallToolResult" as call_tool_result_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::CallToolResult,
    "2024-11-05" / "Tool" as tool_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::Tool,
    "2024-11-05" / "Prompt" as prompt_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::Prompt,
    "2024-11-05" / "PromptMessage" as prompt_message_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::PromptMessage,
    "2024-11-05" / "ResourceContents" as resource_contents_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::ResourceContents,
    "2024-11-05" / "TextContent" as text_content_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::TextContent,
    "2024-11-05" / "ImageContent" as image_content_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::ImageContent,
    "2024-11-05" / "EmbeddedResource" as embedded_resource_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::EmbeddedResource,
    "2024-11-05" / "ListResourcesResult" as list_resources_result_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::ListResourcesResult,
    "2024-11-05" / "ListPromptsResult" as list_prompts_result_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::ListPromptsResult,
    "2024-11-05" / "ReadResourceResult" as read_resource_result_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::ReadResourceResult,
    "2024-11-05" / "LoggingLevel" as logging_level_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::LoggingLevel,
    "2024-11-05" / "ModelPreferences" as model_preferences_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::ModelPreferences,
    "2024-11-05" / "ClientCapabilities" as client_capabilities_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::ClientCapabilities,
    "2024-11-05" / "ServerCapabilities" as server_capabilities_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::ServerCapabilities,
    "2024-11-05" / "SamplingMessage" as sampling_message_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::SamplingMessage,
    "2024-11-05" / "CompleteRequest" as complete_request_2024_11_05
        : protocol_droid_schema::wire::v2024_11_05::CompleteRequest,

    // 2025-03-26. First revision with `ToolAnnotations` and `AudioContent`.
    "2025-03-26" / "CallToolResult" as call_tool_result_2025_03_26
        : protocol_droid_schema::wire::v2025_03_26::CallToolResult,
    "2025-03-26" / "ToolAnnotations" as tool_annotations_2025_03_26
        : protocol_droid_schema::wire::v2025_03_26::ToolAnnotations,
    "2025-03-26" / "AudioContent" as audio_content_2025_03_26
        : protocol_droid_schema::wire::v2025_03_26::AudioContent,
    "2025-03-26" / "ModelHint" as model_hint_2025_03_26
        : protocol_droid_schema::wire::v2025_03_26::ModelHint,
    "2025-03-26" / "ModelPreferences" as model_preferences_2025_03_26
        : protocol_droid_schema::wire::v2025_03_26::ModelPreferences,
    "2025-03-26" / "CompleteResult" as complete_result_2025_03_26
        : protocol_droid_schema::wire::v2025_03_26::CompleteResult,

    // 2025-06-18. First revision with the `ContentBlock` union and roots.
    "2025-06-18" / "ContentBlock" as content_block_2025_06_18
        : protocol_droid_schema::wire::v2025_06_18::ContentBlock,
    "2025-06-18" / "CallToolResult" as call_tool_result_2025_06_18
        : protocol_droid_schema::wire::v2025_06_18::CallToolResult,
    "2025-06-18" / "ToolAnnotations" as tool_annotations_2025_06_18
        : protocol_droid_schema::wire::v2025_06_18::ToolAnnotations,
    "2025-06-18" / "ListRootsResult" as list_roots_result_2025_06_18
        : protocol_droid_schema::wire::v2025_06_18::ListRootsResult,
    "2025-06-18" / "Root" as root_2025_06_18
        : protocol_droid_schema::wire::v2025_06_18::Root,
    "2025-06-18" / "CreateMessageResult" as create_message_result_2025_06_18
        : protocol_droid_schema::wire::v2025_06_18::CreateMessageResult,

    // 2025-11-25. First revision with elicitation, tasks, and the subschema
    // definitions (`NumberSchema`, `StringSchema`, ...) that make a recursive
    // schema-shaped value rather than a message.
    "2025-11-25" / "ContentBlock" as content_block_2025_11_25
        : protocol_droid_schema::wire::v2025_11_25::ContentBlock,
    "2025-11-25" / "CallToolResult" as call_tool_result_2025_11_25
        : protocol_droid_schema::wire::v2025_11_25::CallToolResult,
    "2025-11-25" / "ElicitRequest" as elicit_request_2025_11_25
        : protocol_droid_schema::wire::v2025_11_25::ElicitRequest,
    "2025-11-25" / "ElicitResult" as elicit_result_2025_11_25
        : protocol_droid_schema::wire::v2025_11_25::ElicitResult,
    "2025-11-25" / "ElicitRequestFormParams" as elicit_form_2025_11_25
        : protocol_droid_schema::wire::v2025_11_25::ElicitRequestFormParams,
    "2025-11-25" / "CancelTaskRequest" as cancel_task_request_2025_11_25
        : protocol_droid_schema::wire::v2025_11_25::CancelTaskRequest,
    "2025-11-25" / "Icon" as icon_2025_11_25
        : protocol_droid_schema::wire::v2025_11_25::Icon,
    "2025-11-25" / "ClientCapabilities" as client_capabilities_2025_11_25
        : protocol_droid_schema::wire::v2025_11_25::ClientCapabilities,

    // 2026-07-28 and `draft`. The recursive `JSONValue` family lives here, so
    // these are the cases that exercise the depth bound in the value generator.
    "2026-07-28" / "JSONValue" as json_value_2026_07_28
        : protocol_droid_schema::wire::v2026_07_28::JSONValue,
    "2026-07-28" / "JSONObject" as json_object_2026_07_28
        : protocol_droid_schema::wire::v2026_07_28::JSONObject,
    "2026-07-28" / "JSONArray" as json_array_2026_07_28
        : protocol_droid_schema::wire::v2026_07_28::JSONArray,
    "2026-07-28" / "JSONRPCMessage" as jsonrpc_message_2026_07_28
        : protocol_droid_schema::wire::v2026_07_28::JSONRPCMessage,
    "2026-07-28" / "ContentBlock" as content_block_2026_07_28
        : protocol_droid_schema::wire::v2026_07_28::ContentBlock,
    "2026-07-28" / "ToolAnnotations" as tool_annotations_2026_07_28
        : protocol_droid_schema::wire::v2026_07_28::ToolAnnotations,
    "2026-07-28" / "ToolChoice" as tool_choice_2026_07_28
        : protocol_droid_schema::wire::v2026_07_28::ToolChoice,
    "2026-07-28" / "ToolResultContent" as tool_result_content_2026_07_28
        : protocol_droid_schema::wire::v2026_07_28::ToolResultContent,
    "2026-07-28" / "ElicitRequest" as elicit_request_2026_07_28
        : protocol_droid_schema::wire::v2026_07_28::ElicitRequest,
    "2026-07-28" / "Icons" as icons_2026_07_28
        : protocol_droid_schema::wire::v2026_07_28::Icons,
    "2026-07-28" / "SubscriptionsListenResult" as listen_result_2026_07_28
        : protocol_droid_schema::wire::v2026_07_28::SubscriptionsListenResult,
    "draft" / "JSONValue" as json_value_draft
        : protocol_droid_schema::wire::draft::JSONValue,
    "draft" / "JSONRPCMessage" as jsonrpc_message_draft
        : protocol_droid_schema::wire::draft::JSONRPCMessage,
    "draft" / "ContentBlock" as content_block_draft
        : protocol_droid_schema::wire::draft::ContentBlock,
    "draft" / "CallToolResult" as call_tool_result_draft
        : protocol_droid_schema::wire::draft::CallToolResult,
    "draft" / "ToolChoice" as tool_choice_draft
        : protocol_droid_schema::wire::draft::ToolChoice,
    "draft" / "Icon" as icon_draft
        : protocol_droid_schema::wire::draft::Icon,
    "draft" / "ElicitRequestFormParams" as elicit_form_draft
        : protocol_droid_schema::wire::draft::ElicitRequestFormParams,
    "draft" / "ServerCapabilities" as server_capabilities_draft
        : protocol_droid_schema::wire::draft::ServerCapabilities,
}
