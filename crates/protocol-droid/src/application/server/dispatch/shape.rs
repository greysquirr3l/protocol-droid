//! Building the result document.
//!
//! A dispatched method answers with an IR-level [`JsonObject`], not a wire
//! frame — the codec lowers it for whichever revision negotiated. These are
//! the helpers that build that object: the envelope every result shares, the
//! per-entry conversion for a listing, and the argument list on a prompt.
//!
//! Kept apart from the router for the same reason as the parameter readers:
//! "this listing entry has the wrong field" is a different failure from "this
//! method name routed to the wrong handler", and they should not share a file.

use super::{Cacheable, result_envelope_defined_in};
use crate::application::registry::listing::Listing;
use crate::domain::ir::JsonObject;
use crate::domain::registry::{Primitive, PromptArgument, Registered};
use crate::domain::ProtocolVersion;

/// Adds the members **2026-07-28** requires on a result.
///
/// Two SEPs, one place:
///
/// - `resultType` is required on *every* result at this revision. Every
///   definition in the vendored schema that ends in `Result` lists it, so a
///   result without one is schema-invalid rather than merely dated.
/// - `ttlMs` and `cacheScope` are required only on the cacheable ones —
///   the four listings and `resources/read`. A `CallToolResult` carrying a
///   `ttlMs` would be telling the client to reuse a side effect.
///
/// Older revisions are untouched: both members are absent from every
/// pre-2026 result definition, and adding them would make this crate's own
/// answer invalid on the four revisions it already passes.
///
/// `ttlMs` is `0` and `cacheScope` is `private` for the same reasons as in
/// [`Dispatcher::discover`]: this server holds no cache, so the only lifetime
/// it can honour is "not at all", and the answer describes a registry whose
/// contents can change under a caller, which is not shareable across
/// authorization contexts.
pub(super) fn stamp_result(result: JsonObject, cacheable: Cacheable, version: ProtocolVersion) -> JsonObject {
    if !result_envelope_defined_in(version) {
        return result;
    }
    let mut merged: serde_json::Map<String, serde_json::Value> =
        match serde_json::from_str(result.as_str()) {
            Ok(serde_json::Value::Object(members)) => members,
            _ => return result,
        };
    // A result that already names its type keeps it: an interim
    // `InputRequiredResult` says `"input_required"`, and overwriting that with
    // `"complete"` would tell the client the request finished.
    merged
        .entry("resultType")
        .or_insert_with(|| serde_json::Value::from("complete"));
    if cacheable == Cacheable::Yes {
        merged.insert("ttlMs".to_owned(), serde_json::Value::from(0));
        merged.insert("cacheScope".to_owned(), serde_json::Value::from("private"));
    }
    JsonObject::from_value(serde_json::Value::Object(merged)).unwrap_or(result)
}


/// Drops members whose value is `null`.
///
/// Every optional member of a listing entry is typed as a string, a boolean or
/// an object in every revision's schema, and none admits `null`. Writing an
/// absent description as `"description": null` is therefore a schema-invalid
/// entry, and a client that validates its input — the `2026-07-28` reference
/// client does — refuses the whole listing over it. Only the top level is
/// touched: a tool's `inputSchema` is the tool author's document, and `null`
/// is a legitimate value inside one.
pub(super) fn without_nulls(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(members) = value.as_object_mut() {
        members.retain(|_, member| !member.is_null());
    }
    value
}


pub(super) fn wrap_listing(key: &str, listing: &Listing) -> JsonObject {
    let entries: Vec<serde_json::Value> = listing
        .visible()
        .iter()
        .map(|entry| without_nulls(listing_entry(entry)))
        .collect();
    JsonObject::from_value(serde_json::json!({ key: entries })).unwrap_or_default()
}


/// The common-denominator JSON for one registered primitive.
///
/// The match is exhaustive, which is the whole point: a new `Primitive` variant
/// fails to compile in this function rather than silently vanishing from every
/// listing. That is what the exhaustiveness buys, and it needs no `Option` to
/// buy it — an `Option` here would only invite a `filter_map` to swallow a
/// future `None` quietly, undoing the check.
pub(super) fn listing_entry(registered: &Registered) -> serde_json::Value {
    match registered.primitive() {
        Primitive::Tool(tool) => {
            let mut entry = serde_json::json!({
                "name": tool.name.as_str(),
                "description": tool.description,
            });
            if let (serde_json::Value::Object(map), Ok(schema)) = (
                &mut entry,
                serde_json::from_str::<serde_json::Value>(tool.input_schema.as_str()),
            ) {
                // Embedded as a value, not as a string. `json!` will not do
                // this for us: `"inputSchema": text.as_str()` produces a JSON
                // *string* whose characters happen to be JSON, so every peer
                // would read a schema where the specification says an object,
                // and the failure would surface in the client as a tool with
                // no arguments rather than here.
                map.insert("inputSchema".to_string(), schema);
            }
            entry
        }
        Primitive::Resource(resource) => serde_json::json!({
            "uri": resource.uri.as_str(),
            "name": resource.name,
            "description": resource.description,
            "mimeType": resource.mime_type,
        }),
        Primitive::ResourceTemplate(template) => serde_json::json!({
            "uriTemplate": template.uri_template.as_str(),
            "name": template.name,
            "description": template.description,
            "mimeType": template.mime_type,
        }),
        Primitive::Prompt(prompt) => serde_json::json!({
            "name": prompt.name.as_str(),
            "description": prompt.description,
            "arguments": prompt.arguments.iter().map(prompt_argument).collect::<Vec<_>>(),
        }),
    }
}


/// One prompt argument, as JSON.
///
/// Written out rather than derived, because `PromptArgument` is a domain type
/// with no `Serialize`: it exists to be lowered by a codec, and deriving
/// `Serialize` on it here would make the wire shape the domain's own opinion
/// instead of the codec's. `title` is skipped for the same reason the
/// other listings skip it — `2026-07-28` only.
pub(super) fn prompt_argument(argument: &PromptArgument) -> serde_json::Value {
    without_nulls(serde_json::json!({
        "name": argument.name,
        "description": argument.description,
        "required": argument.required,
    }))
}

