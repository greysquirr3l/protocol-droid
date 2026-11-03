//! Reading fields out of a parsed manual.
//!
//! # Why these are helpers and not a derive
//!
//! The manual is a *document*, and the parts of it this crate models
//! precisely — the version fields, the tool names, the call template types —
//! are the parts a client acts on. The parts it does not model precisely (a
//! JSON Schema, an auth block for a protocol this build has never heard of)
//! are kept as they arrived. A derive would have to pick one behaviour for
//! every field, and the two kinds of field want different ones: a missing
//! tool name is a broken manual, and a missing schema keyword is a schema
//! this build does not know.
//!
//! So the manual is parsed by hand, once, at the boundary. Everything
//! downstream holds typed values.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::domain::ir::JsonObject;
use crate::domain::utcp::error::ManualError;
use crate::domain::utcp::version::UtcpVersion;

/// The object half of a `Value`.
pub fn object<'a>(value: &'a Value, what: &'static str) -> Result<&'a Map<String, Value>, ManualError> {
    value
        .as_object()
        .ok_or(ManualError::NotAnObject { what })
}

/// A required string field.
pub fn required_str(
    object: &Map<String, Value>,
    what: &'static str,
    field: &'static str,
) -> Result<String, ManualError> {
    let value = object
        .get(field)
        .ok_or(ManualError::MissingField { what, field })?;
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| ManualError::InvalidField {
            what,
            field,
            detail: "it is not a string".to_owned(),
        })
}

/// An optional string field.
pub fn optional_str(object: &Map<String, Value>, field: &str) -> Option<String> {
    object.get(field).and_then(Value::as_str).map(str::to_owned)
}

/// A required version field.
pub fn required_version(
    object: &Map<String, Value>,
    what: &'static str,
    field: &'static str,
) -> Result<UtcpVersion, ManualError> {
    let raw = required_str(object, what, field)?;
    raw.parse::<UtcpVersion>().map_err(|source| ManualError::InvalidVersion {
        what,
        field,
        raw: raw.clone(),
        source,
    })
}

/// An optional JSON object field.
pub fn optional_json_object(
    object: &Map<String, Value>,
    what: &'static str,
    field: &'static str,
) -> Result<Option<JsonObject>, ManualError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => JsonObject::from_value(value.clone())
            .map(Some)
            .map_err(|error| ManualError::InvalidField {
                what,
                field,
                detail: error.to_string(),
            }),
    }
}

/// A list of strings, empty when the field is absent.
pub fn string_array(object: &Map<String, Value>, what: &'static str, field: &'static str) -> Result<Vec<String>, ManualError> {
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(ManualError::InvalidField {
            what,
            field,
            detail: "it is not a list".to_owned(),
        });
    };
    items
        .iter()
        .map(|item| {
            item.as_str().map(str::to_owned).ok_or_else(|| {
                ManualError::InvalidField {
                    what,
                    field,
                    detail: "one of its entries is not a string".to_owned(),
                }
            })
        })
        .collect()
}

/// A map of strings, empty when the field is absent.
pub fn string_map(
    object: &Map<String, Value>,
    what: &'static str,
    field: &'static str,
) -> Result<BTreeMap<String, String>, ManualError> {
    let Some(value) = object.get(field) else {
        return Ok(BTreeMap::new());
    };
    let Some(entries) = value.as_object() else {
        return Err(ManualError::InvalidField {
            what,
            field,
            detail: "it is not an object".to_owned(),
        });
    };
    entries
        .iter()
        .map(|(key, item)| {
            item.as_str()
                .map(|value| (key.clone(), value.to_owned()))
                .ok_or_else(|| ManualError::InvalidField {
                    what,
                    field,
                    detail: format!("the entry `{key}` is not a string"),
                })
        })
        .collect()
}
