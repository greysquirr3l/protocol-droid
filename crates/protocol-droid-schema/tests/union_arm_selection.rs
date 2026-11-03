//! The alternative a union selects, for every union in every revision.
//!
//! A union whose alternatives are told apart by a pinned member — a request's
//! `method`, a content block's `type` — must select the alternative the peer
//! actually sent. A union whose alternatives overlap — the result unions, the
//! schema unions — must select the alternative that accounts for more of the
//! value.
//!
//! Matching alternatives in the specification's own order gets both wrong, and
//! gets them wrong *silently*: every generated type carries a catch-all for the
//! members it does not name, so a value decoded into the wrong alternative
//! still round-trips byte for byte. A round-trip test cannot see this. The
//! test therefore looks at *which* alternative was selected.
//!
//! The value is built from the schema, not written by hand, so a revision that
//! gains an alternative is covered without anyone remembering to add a case.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use protocol_droid_schema::wire::{
    draft, v2024_11_05, v2025_03_26, v2025_06_18, v2025_11_25, v2026_07_28,
};
use serde_json::{Map, Value};

/// The definitions container of a schema, keyed by definition name.
type Defs = BTreeMap<String, Value>;

/// How deep the canonical-value walk descends.
///
/// The schemas are recursive — `JSONRPCMessage` contains `Vec<JSONRPCMessage>`
/// — so an unbounded walk does not terminate. The bound is deep enough for the
/// longest chain the vendored schemas actually have: a result holding a
/// message holding a content block holding its annotations.
const MAX_DEPTH: usize = 12;

/// A failure, so the test can report which union and which alternative
/// disagreed rather than only that something did.
#[derive(Debug)]
enum Failure {
    /// The union named by a case is not in the schema.
    Missing(String),
    /// No value could be built for an alternative.
    NoValue(String),
    /// The value did not decode at all.
    Decode(String),
    /// The value decoded, but into the wrong alternative.
    WrongArm(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(message) => write!(formatter, "missing union: {message}"),
            Self::NoValue(message) => write!(formatter, "no value built: {message}"),
            Self::Decode(message) => write!(formatter, "decode failed: {message}"),
            Self::WrongArm(message) => write!(formatter, "wrong alternative: {message}"),
        }
    }
}

/// The directory the vendored specifications live in.
fn spec_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map_or_else(PathBuf::new, |root| {
            root.join("crates/protocol-droid-schema/spec")
        })
}

/// The parsed `schema.json` for one revision.
fn schema_for(revision: &str) -> Value {
    let path = spec_dir().join(revision).join("schema.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// The definitions of one revision, keyed by name.
fn defs_for(revision: &str) -> Defs {
    let schema = schema_for(revision);
    schema
        .get("$defs")
        .or_else(|| schema.get("definitions"))
        .and_then(Value::as_object)
        .map(|members| {
            members
                .iter()
                .map(|(name, body)| (name.clone(), body.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The schema node a JSON pointer names.
///
/// The pointer is the path to the union inside the revision's schema
/// document, which is how the test finds the same alternatives the generator
/// did — the Rust type name alone cannot be walked back to the schema.
///
/// A segment that names an index walks into an array, which is how a union
/// nested inside a union member's elements is reached.
fn resolve<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    let mut current = root;
    for segment in pointer.split('/').filter(|segment| !segment.is_empty()) {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        current = match current {
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => current.get(&segment)?,
        };
    }
    Some(current)
}

/// The value that best identifies one alternative of a union.
///
/// Every member the alternative *requires* is present, and so is every member
/// it pins to a constant or a closed set of values. Those are the only
/// members that identify the alternative: they are what the specification
/// says must be there, and what it says must have that value.
///
/// A member that is optional and not pinned is left out. Including it would
/// make the value look like a *different* alternative's value — `Result`
/// names `_meta` and so does `SubscriptionsListenResult`, and only the
/// members one of them requires tell them apart.
///
/// A member the schema does not require is left out when no value can be built
/// for it. A member it *does* require is not: dropping that would produce a
/// value the schema rejects, and the alternative it was built for would then
/// fail to decode and the dispatcher would fall through to another one.
fn canonical(schema: &Value, defs: &Defs, depth: usize) -> Option<Value> {
    if depth > MAX_DEPTH {
        return None;
    }
    if let Some(value) = schema.get("const") {
        return Some(value.clone());
    }
    if let Some(Value::Array(values)) = schema.get("enum") {
        return values.first().cloned();
    }
    if let Some(reference) = reference_of(schema) {
        return canonical(defs.get(reference)?, defs, depth + 1);
    }
    if let Some(Value::Array(branches)) = schema.get("oneOf").or_else(|| schema.get("anyOf")) {
        return canonical(branches.first()?, defs, depth + 1);
    }

    let properties = declared_properties(schema, defs);
    if !properties.is_empty() {
        let required = declared_required(schema, defs);
        let mut object = Map::new();
        for (name, property) in &properties {
            if !required.contains(name) && !is_pinned(property) {
                continue;
            }
            match canonical(property, defs, depth + 1) {
                Some(value) => {
                    object.insert(name.clone(), value);
                }
                None if required.contains(name) => return None,
                None => {}
            }
        }
        return Some(Value::Object(object));
    }

    if let Some(items) = schema.get("items") {
        let element = canonical(items, defs, depth + 1)?;
        return Some(Value::Array(vec![element]));
    }

    scalar_of(schema)
}

/// True when a property carries a value the specification fixes.
///
/// A property pinned to a constant, or restricted to a closed set of them, is
/// part of what identifies the alternative it belongs to.
fn is_pinned(property: &Value) -> bool {
    property.get("const").is_some()
        || matches!(property.get("enum"), Some(Value::Array(values)) if !values.is_empty())
}

/// The properties a schema declares, including those its `allOf` parts
/// declare.
///
/// The specification composes a base result with a payload that way, and the
/// payload's members are what tell two alternatives apart.
fn declared_properties(schema: &Value, defs: &Defs) -> Map<String, Value> {
    let mut properties = match schema.get("properties") {
        Some(Value::Object(declared)) => declared.clone(),
        _ => Map::new(),
    };
    for part in all_of(schema, defs) {
        if let Some(Value::Object(declared)) = part.get("properties") {
            for (name, property) in declared {
                properties.insert(name.clone(), property.clone());
            }
        }
    }
    properties
}

/// The members a schema requires, including those its `allOf` parts require.
fn declared_required(schema: &Value, defs: &Defs) -> Vec<String> {
    let mut required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    for part in all_of(schema, defs) {
        if let Some(Value::Array(names)) = part.get("required") {
            for name in names.iter().filter_map(Value::as_str) {
                let name = name.to_owned();
                if !required.contains(&name) {
                    required.push(name);
                }
            }
        }
    }
    required
}

/// The parts a schema's `allOf` names, with references resolved.
fn all_of<'a>(schema: &'a Value, defs: &'a Defs) -> Vec<&'a Value> {
    let Some(Value::Array(parts)) = schema.get("allOf") else {
        return Vec::new();
    };
    parts
        .iter()
        .map(|part| {
            reference_of(part).map_or(part, |reference| defs.get(reference).unwrap_or(part))
        })
        .collect()
}

/// The definition a `$ref` names, if the schema carries one.
fn reference_of(schema: &Value) -> Option<&str> {
    let reference = schema.get("$ref")?.as_str()?;
    reference.rsplit('/').next()
}

/// A minimal value for a schema that names no properties.
fn scalar_of(schema: &Value) -> Option<Value> {
    let Some(declared) = schema.get("type") else {
        // A schema that names no type constrains nothing, so any JSON value
        // satisfies it. `null` is the one that carries no structure, which
        // keeps the value small.
        return Some(Value::Null);
    };
    let name = match declared {
        Value::String(text) => text.as_str(),
        Value::Array(names) => names.first()?.as_str()?,
        _ => return None,
    };
    match name {
        "string" => Some(Value::String(String::new())),
        "integer" | "number" => Some(Value::Number(0.into())),
        "boolean" => Some(Value::Bool(false)),
        "array" => Some(Value::Array(Vec::new())),
        "object" => Some(Value::Object(Map::new())),
        "null" => Some(Value::Null),
        _ => None,
    }
}

/// The alternative the dispatcher selected for a value.
///
/// The generated types name their alternatives `Member0`, `Member1` and so
/// on, in the specification's order, so the selected alternative is read off
/// the debug output rather than matched against a variant list.
fn selected_arm(decoded: &str) -> Option<usize> {
    let rest = decoded.strip_prefix("Member")?;
    let (digits, _) = rest.split_once('(')?;
    digits.parse().ok()
}

/// Checks that every alternative of a union is the one its own value selects.
///
/// Two alternatives that produce the *same* value cannot be told apart by
/// anything: no dispatcher, however clever, can put that value in one arm and
/// not the other. The specification contains four such pairs, all of them
/// results that differ only in members the schema does not require. For those
/// the test asserts the weaker invariant that still holds — the selected
/// alternative is one that accepts the value — and for everything else it
/// asserts the exact alternative.
fn check_union(
    revision: &str,
    pointer: &str,
    probe: impl Fn(&Value) -> Result<String, Failure>,
) -> Result<(), Failure> {
    let schema = schema_for(revision);
    let defs = defs_for(revision);
    let union = resolve(&schema, pointer)
        .ok_or_else(|| Failure::Missing(format!("{revision}: no schema at {pointer}")))?;
    let alternatives = union
        .as_array()
        .ok_or_else(|| Failure::Missing(format!("{revision}: {pointer} is not a union")))?;

    let values: Vec<Option<Value>> = alternatives
        .iter()
        .map(|alternative| canonical(alternative, &defs, 0))
        .collect();

    for (index, value) in values.iter().enumerate() {
        let value = value.as_ref().ok_or_else(|| {
            Failure::NoValue(format!(
                "{revision}: no value for alternative {index} of {pointer}"
            ))
        })?;

        // The alternatives that produce a byte-identical value are the ones
        // no dispatcher can separate.
        let acceptable: Vec<usize> = values
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.as_ref() == Some(value))
            .map(|(other, _)| other)
            .collect();
        let ambiguous = acceptable.len() > 1;

        let selected = probe(value)?;
        let arm = selected_arm(&selected).ok_or_else(|| {
            Failure::WrongArm(format!(
                "{revision}: {pointer} alternative {index} decoded as {selected}, \
                 which names no alternative"
            ))
        })?;

        if ambiguous {
            // The value belongs to several alternatives. Whichever one is
            // selected must be one of them, or the dispatcher put a value in
            // an arm that does not accept it.
            if !acceptable.contains(&arm) {
                return Err(Failure::WrongArm(format!(
                    "{revision}: {pointer} alternative {index} selected {selected}, \
                     which accepts no alternative the value fits"
                )));
            }
            continue;
        }

        if arm != index {
            return Err(Failure::WrongArm(format!(
                "{revision}: {pointer} alternative {index} selected {selected} \
                 (expected Member{index})"
            )));
        }
    }
    Ok(())
}

/// Every union in every revision, with the schema pointer that finds it.
macro_rules! unions {
    ($($revision:literal => $module:ident :: $ident:ident : $pointer:literal,)*) => {
        /// Every alternative of every union is selected by its own value.
        #[test]
        fn every_alternative_is_selected_by_its_own_value() -> Result<(), Failure> {
            $(
                {
                    let probe = |value: &Value| -> Result<String, Failure> {
                        let decoded: $module::$ident = serde_json::from_value(value.clone())
                            .map_err(|error| Failure::Decode(format!(
                                "$revision: {}: {error}", stringify!($ident)
                            )))?;
                        Ok(format!("{decoded:?}"))
                    };
                    check_union($revision, $pointer, probe)?;
                }
            )*
            Ok(())
        }
    };
}

unions! {
        "2024-11-05" => v2024_11_05::CallToolResultContentChoice: "/definitions/CallToolResult/properties/content/items/anyOf",
        "2024-11-05" => v2024_11_05::ClientNotificationChoice: "/definitions/ClientNotification/anyOf",
        "2024-11-05" => v2024_11_05::ClientRequestChoice: "/definitions/ClientRequest/anyOf",
        "2024-11-05" => v2024_11_05::ClientResultChoice: "/definitions/ClientResult/anyOf",
        "2024-11-05" => v2024_11_05::CompleteRequestParamsRefChoice: "/definitions/CompleteRequest/properties/params/properties/ref/anyOf",
        "2024-11-05" => v2024_11_05::CreateMessageResultContentChoice: "/definitions/CreateMessageResult/properties/content/anyOf",
        "2024-11-05" => v2024_11_05::EmbeddedResourceResourceChoice: "/definitions/EmbeddedResource/properties/resource/anyOf",
        "2024-11-05" => v2024_11_05::JSONRPCMessageChoice: "/definitions/JSONRPCMessage/anyOf",
        "2024-11-05" => v2024_11_05::PromptMessageContentChoice: "/definitions/PromptMessage/properties/content/anyOf",
        "2024-11-05" => v2024_11_05::ReadResourceResultContentsChoice: "/definitions/ReadResourceResult/properties/contents/items/anyOf",
        "2024-11-05" => v2024_11_05::SamplingMessageContentChoice: "/definitions/SamplingMessage/properties/content/anyOf",
        "2024-11-05" => v2024_11_05::ServerNotificationChoice: "/definitions/ServerNotification/anyOf",
        "2024-11-05" => v2024_11_05::ServerRequestChoice: "/definitions/ServerRequest/anyOf",
        "2024-11-05" => v2024_11_05::ServerResultChoice: "/definitions/ServerResult/anyOf",
        "2025-03-26" => v2025_03_26::CallToolResultContentChoice: "/definitions/CallToolResult/properties/content/items/anyOf",
        "2025-03-26" => v2025_03_26::ClientNotificationChoice: "/definitions/ClientNotification/anyOf",
        "2025-03-26" => v2025_03_26::ClientRequestChoice: "/definitions/ClientRequest/anyOf",
        "2025-03-26" => v2025_03_26::ClientResultChoice: "/definitions/ClientResult/anyOf",
        "2025-03-26" => v2025_03_26::CompleteRequestParamsRefChoice: "/definitions/CompleteRequest/properties/params/properties/ref/anyOf",
        "2025-03-26" => v2025_03_26::CreateMessageResultContentChoice: "/definitions/CreateMessageResult/properties/content/anyOf",
        "2025-03-26" => v2025_03_26::EmbeddedResourceResourceChoice: "/definitions/EmbeddedResource/properties/resource/anyOf",
        "2025-03-26" => v2025_03_26::JSONRPCBatchRequestChoice: "/definitions/JSONRPCBatchRequest/items/anyOf",
        "2025-03-26" => v2025_03_26::JSONRPCBatchResponseChoice: "/definitions/JSONRPCBatchResponse/items/anyOf",
        "2025-03-26" => v2025_03_26::JSONRPCMessageChoice: "/definitions/JSONRPCMessage/anyOf",
        "2025-03-26" => v2025_03_26::JSONRPCMessageM2Choice: "/definitions/JSONRPCMessage/anyOf/2/items/anyOf",
        "2025-03-26" => v2025_03_26::JSONRPCMessageM5Choice: "/definitions/JSONRPCMessage/anyOf/5/items/anyOf",
        "2025-03-26" => v2025_03_26::PromptMessageContentChoice: "/definitions/PromptMessage/properties/content/anyOf",
        "2025-03-26" => v2025_03_26::ReadResourceResultContentsChoice: "/definitions/ReadResourceResult/properties/contents/items/anyOf",
        "2025-03-26" => v2025_03_26::SamplingMessageContentChoice: "/definitions/SamplingMessage/properties/content/anyOf",
        "2025-03-26" => v2025_03_26::ServerNotificationChoice: "/definitions/ServerNotification/anyOf",
        "2025-03-26" => v2025_03_26::ServerRequestChoice: "/definitions/ServerRequest/anyOf",
        "2025-03-26" => v2025_03_26::ServerResultChoice: "/definitions/ServerResult/anyOf",
        "2025-06-18" => v2025_06_18::ClientNotificationChoice: "/definitions/ClientNotification/anyOf",
        "2025-06-18" => v2025_06_18::ClientRequestChoice: "/definitions/ClientRequest/anyOf",
        "2025-06-18" => v2025_06_18::ClientResultChoice: "/definitions/ClientResult/anyOf",
        "2025-06-18" => v2025_06_18::CompleteRequestParamsRefChoice: "/definitions/CompleteRequest/properties/params/properties/ref/anyOf",
        "2025-06-18" => v2025_06_18::ContentBlockChoice: "/definitions/ContentBlock/anyOf",
        "2025-06-18" => v2025_06_18::CreateMessageResultContentChoice: "/definitions/CreateMessageResult/properties/content/anyOf",
        "2025-06-18" => v2025_06_18::EmbeddedResourceResourceChoice: "/definitions/EmbeddedResource/properties/resource/anyOf",
        "2025-06-18" => v2025_06_18::JSONRPCMessageChoice: "/definitions/JSONRPCMessage/anyOf",
        "2025-06-18" => v2025_06_18::PrimitiveSchemaDefinitionChoice: "/definitions/PrimitiveSchemaDefinition/anyOf",
        "2025-06-18" => v2025_06_18::ReadResourceResultContentsChoice: "/definitions/ReadResourceResult/properties/contents/items/anyOf",
        "2025-06-18" => v2025_06_18::SamplingMessageContentChoice: "/definitions/SamplingMessage/properties/content/anyOf",
        "2025-06-18" => v2025_06_18::ServerNotificationChoice: "/definitions/ServerNotification/anyOf",
        "2025-06-18" => v2025_06_18::ServerRequestChoice: "/definitions/ServerRequest/anyOf",
        "2025-06-18" => v2025_06_18::ServerResultChoice: "/definitions/ServerResult/anyOf",
        "2025-11-25" => v2025_11_25::ClientNotificationChoice: "/$defs/ClientNotification/anyOf",
        "2025-11-25" => v2025_11_25::ClientRequestChoice: "/$defs/ClientRequest/anyOf",
        "2025-11-25" => v2025_11_25::ClientResultChoice: "/$defs/ClientResult/anyOf",
        "2025-11-25" => v2025_11_25::CompleteRequestParamsRefChoice: "/$defs/CompleteRequestParams/properties/ref/anyOf",
        "2025-11-25" => v2025_11_25::ContentBlockChoice: "/$defs/ContentBlock/anyOf",
        "2025-11-25" => v2025_11_25::CreateMessageResultContentChoice: "/$defs/CreateMessageResult/properties/content/anyOf",
        "2025-11-25" => v2025_11_25::ElicitRequestParamsChoice: "/$defs/ElicitRequestParams/anyOf",
        "2025-11-25" => v2025_11_25::EmbeddedResourceResourceChoice: "/$defs/EmbeddedResource/properties/resource/anyOf",
        "2025-11-25" => v2025_11_25::EnumSchemaChoice: "/$defs/EnumSchema/anyOf",
        "2025-11-25" => v2025_11_25::JSONRPCMessageChoice: "/$defs/JSONRPCMessage/anyOf",
        "2025-11-25" => v2025_11_25::JSONRPCResponseChoice: "/$defs/JSONRPCResponse/anyOf",
        "2025-11-25" => v2025_11_25::MultiSelectEnumSchemaChoice: "/$defs/MultiSelectEnumSchema/anyOf",
        "2025-11-25" => v2025_11_25::PrimitiveSchemaDefinitionChoice: "/$defs/PrimitiveSchemaDefinition/anyOf",
        "2025-11-25" => v2025_11_25::ReadResourceResultContentsChoice: "/$defs/ReadResourceResult/properties/contents/items/anyOf",
        "2025-11-25" => v2025_11_25::SamplingMessageContentChoice: "/$defs/SamplingMessage/properties/content/anyOf",
        "2025-11-25" => v2025_11_25::SamplingMessageContentBlockChoice: "/$defs/SamplingMessageContentBlock/anyOf",
        "2025-11-25" => v2025_11_25::ServerNotificationChoice: "/$defs/ServerNotification/anyOf",
        "2025-11-25" => v2025_11_25::ServerRequestChoice: "/$defs/ServerRequest/anyOf",
        "2025-11-25" => v2025_11_25::ServerResultChoice: "/$defs/ServerResult/anyOf",
        "2025-11-25" => v2025_11_25::SingleSelectEnumSchemaChoice: "/$defs/SingleSelectEnumSchema/anyOf",
        "2026-07-28" => v2026_07_28::CallToolResultResponseResultChoice: "/$defs/CallToolResultResponse/properties/result/anyOf",
        "2026-07-28" => v2026_07_28::ClientRequestChoice: "/$defs/ClientRequest/anyOf",
        "2026-07-28" => v2026_07_28::CompleteRequestParamsRefChoice: "/$defs/CompleteRequestParams/properties/ref/anyOf",
        "2026-07-28" => v2026_07_28::ContentBlockChoice: "/$defs/ContentBlock/anyOf",
        "2026-07-28" => v2026_07_28::CreateMessageResultContentChoice: "/$defs/CreateMessageResult/properties/content/anyOf",
        "2026-07-28" => v2026_07_28::ElicitRequestParamsChoice: "/$defs/ElicitRequestParams/anyOf",
        "2026-07-28" => v2026_07_28::EmbeddedResourceResourceChoice: "/$defs/EmbeddedResource/properties/resource/anyOf",
        "2026-07-28" => v2026_07_28::EnumSchemaChoice: "/$defs/EnumSchema/anyOf",
        "2026-07-28" => v2026_07_28::GetPromptResultResponseResultChoice: "/$defs/GetPromptResultResponse/properties/result/anyOf",
        "2026-07-28" => v2026_07_28::InputRequestChoice: "/$defs/InputRequest/anyOf",
        "2026-07-28" => v2026_07_28::InputResponseChoice: "/$defs/InputResponse/anyOf",
        "2026-07-28" => v2026_07_28::JSONRPCMessageChoice: "/$defs/JSONRPCMessage/anyOf",
        "2026-07-28" => v2026_07_28::JSONRPCResponseChoice: "/$defs/JSONRPCResponse/anyOf",
        "2026-07-28" => v2026_07_28::JSONValueChoice: "/$defs/JSONValue/anyOf",
        "2026-07-28" => v2026_07_28::MultiSelectEnumSchemaChoice: "/$defs/MultiSelectEnumSchema/anyOf",
        "2026-07-28" => v2026_07_28::PrimitiveSchemaDefinitionChoice: "/$defs/PrimitiveSchemaDefinition/anyOf",
        "2026-07-28" => v2026_07_28::ReadResourceResultContentsChoice: "/$defs/ReadResourceResult/properties/contents/items/anyOf",
        "2026-07-28" => v2026_07_28::ReadResourceResultResponseResultChoice: "/$defs/ReadResourceResultResponse/properties/result/anyOf",
        "2026-07-28" => v2026_07_28::SamplingMessageContentChoice: "/$defs/SamplingMessage/properties/content/anyOf",
        "2026-07-28" => v2026_07_28::SamplingMessageContentBlockChoice: "/$defs/SamplingMessageContentBlock/anyOf",
        "2026-07-28" => v2026_07_28::ServerNotificationChoice: "/$defs/ServerNotification/anyOf",
        "2026-07-28" => v2026_07_28::ServerResultChoice: "/$defs/ServerResult/anyOf",
        "2026-07-28" => v2026_07_28::SingleSelectEnumSchemaChoice: "/$defs/SingleSelectEnumSchema/anyOf",
        "draft" => draft::CallToolResultResponseResultChoice: "/$defs/CallToolResultResponse/properties/result/anyOf",
        "draft" => draft::ClientRequestChoice: "/$defs/ClientRequest/anyOf",
        "draft" => draft::CompleteRequestParamsRefChoice: "/$defs/CompleteRequestParams/properties/ref/anyOf",
        "draft" => draft::ContentBlockChoice: "/$defs/ContentBlock/anyOf",
        "draft" => draft::CreateMessageResultContentChoice: "/$defs/CreateMessageResult/properties/content/anyOf",
        "draft" => draft::ElicitRequestParamsChoice: "/$defs/ElicitRequestParams/anyOf",
        "draft" => draft::EmbeddedResourceResourceChoice: "/$defs/EmbeddedResource/properties/resource/anyOf",
        "draft" => draft::EnumSchemaChoice: "/$defs/EnumSchema/anyOf",
        "draft" => draft::GetPromptResultResponseResultChoice: "/$defs/GetPromptResultResponse/properties/result/anyOf",
        "draft" => draft::InputRequestChoice: "/$defs/InputRequest/anyOf",
        "draft" => draft::InputResponseChoice: "/$defs/InputResponse/anyOf",
        "draft" => draft::JSONRPCMessageChoice: "/$defs/JSONRPCMessage/anyOf",
        "draft" => draft::JSONRPCResponseChoice: "/$defs/JSONRPCResponse/anyOf",
        "draft" => draft::JSONValueChoice: "/$defs/JSONValue/anyOf",
        "draft" => draft::MultiSelectEnumSchemaChoice: "/$defs/MultiSelectEnumSchema/anyOf",
        "draft" => draft::PrimitiveSchemaDefinitionChoice: "/$defs/PrimitiveSchemaDefinition/anyOf",
        "draft" => draft::ReadResourceResultContentsChoice: "/$defs/ReadResourceResult/properties/contents/items/anyOf",
        "draft" => draft::ReadResourceResultResponseResultChoice: "/$defs/ReadResourceResultResponse/properties/result/anyOf",
        "draft" => draft::SamplingMessageContentChoice: "/$defs/SamplingMessage/properties/content/anyOf",
        "draft" => draft::SamplingMessageContentBlockChoice: "/$defs/SamplingMessageContentBlock/anyOf",
        "draft" => draft::ServerNotificationChoice: "/$defs/ServerNotification/anyOf",
        "draft" => draft::ServerResultChoice: "/$defs/ServerResult/anyOf",
        "draft" => draft::SingleSelectEnumSchemaChoice: "/$defs/SingleSelectEnumSchema/anyOf",
}
