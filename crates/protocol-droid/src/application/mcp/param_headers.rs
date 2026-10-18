//! `Mcp-Param-{Name}`: tool arguments a client mirrors into HTTP headers.
//!
//! `2026-07-28` (SEP-2243). A tool's `inputSchema` may mark a property with
//! `x-mcp-header: "Region"`, and a client calling that tool over Streamable
//! HTTP must then send the argument's value as `Mcp-Param-Region`. A server
//! that reads the body must check the two agree — an intermediary may have
//! routed on the header while this server is about to act on the body — and
//! refuse a disagreement with `400` and `-32020 HeaderMismatch`.
//!
//! The rules here are the transport specification's, quoted where they decide
//! something:
//! <https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http#custom-headers-from-tool-parameters>
//!
//! - Only properties *statically reachable* through `properties` keys count;
//!   an annotation under `items`, a composition keyword, a conditional or a
//!   `$ref` makes the annotation invalid, and an invalid one is not enforced.
//! - Only `string`, `integer` and `boolean` parameters may be annotated.
//! - "Parameter value is `null`" and "parameter not in arguments": the server
//!   "MUST NOT expect the header". "Client omits header but value is in body":
//!   the server "MUST reject the request".
//! - A value wrapped as `=?base64?…?=` is decoded before comparison, and one
//!   whose payload is not valid Base64 is refused; a value without the exact
//!   wrapper is literal.
//! - Integers compare numerically, so `42.0` and `42` agree.

use base64::Engine as _;
use serde_json::Value;

/// One annotated parameter: where its value lives and which header carries it.
struct Annotated {
    path: Vec<String>,
    header: String,
    kind: Kind,
}

#[derive(Clone, Copy)]
enum Kind {
    String,
    Integer,
    Boolean,
}

/// Whether `x-mcp-header`'s value is a legal header-name token (RFC 9110 `tchar`).
fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^'
                        | b'_' | b'`' | b'|' | b'~'
                )
        })
}

/// Whether `schema` carries any `x-mcp-header` annotation the transport
/// specification forbids.
///
/// For a client: "Clients using the Streamable HTTP transport MUST reject tool
/// definitions where any `x-mcp-header` value violates these constraints",
/// which means leaving the tool out of `tools/list`. An annotation is invalid
/// when its name is not a header token, when it sits on a type other than
/// `string`, `integer` or `boolean`, when it is reached through anything but
/// `properties`, or when two annotations name the same header.
#[must_use]
pub fn has_invalid_annotation(schema: &Value) -> bool {
    let reachable = annotations(schema).len();
    let mut everywhere = 0;
    count_anywhere(schema, &mut everywhere);
    reachable != everywhere
}

fn count_anywhere(value: &Value, count: &mut usize) {
    match value {
        Value::Object(members) => {
            if members.get("x-mcp-header").is_some() {
                *count += 1;
            }
            for member in members.values() {
                count_anywhere(member, count);
            }
        }
        Value::Array(items) => {
            for item in items {
                count_anywhere(item, count);
            }
        }
        _ => {}
    }
}

/// The `Mcp-Param-*` headers a client must send for `arguments`.
///
/// One per annotated parameter with a non-null value, encoded per the value
/// rules: strings as-is, integers in decimal, booleans as `true`/`false`, and
/// anything that is not safely a plain header value in the Base64 sentinel.
#[must_use]
pub fn headers_for(schema: &Value, arguments: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for annotated in annotations(schema) {
        let mut value = Some(arguments);
        for key in &annotated.path {
            value = value.and_then(|current| current.get(key));
        }
        let text = match (annotated.kind, value) {
            (Kind::String, Some(Value::String(text))) => text.clone(),
            (Kind::Integer, Some(Value::Number(number))) => number
                .as_i64()
                .map_or_else(|| number.to_string(), |integer| integer.to_string()),
            (Kind::Boolean, Some(Value::Bool(flag))) => flag.to_string(),
            _ => continue,
        };
        out.push((format!("Mcp-Param-{}", annotated.header), encode(&text)));
    }
    out
}

/// Encodes a value for a header that admits the Base64 sentinel.
///
/// Plain when every character is visible ASCII or an inner space or tab;
/// otherwise — and always when the plain text would itself look like the
/// sentinel — `=?base64?{standard Base64 of the UTF-8}?=`.
#[must_use]
pub fn encode(value: &str) -> String {
    let printable = value
        .bytes()
        .all(|byte| byte == b'\t' || (0x20..=0x7e).contains(&byte));
    let padded = value.starts_with([' ', '\t']) || value.ends_with([' ', '\t']);
    let sentinel = value.starts_with("=?base64?") && value.ends_with("?=");
    let safe = printable && !padded && !sentinel;
    if safe {
        return value.to_owned();
    }
    format!(
        "=?base64?{}?=",
        base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
    )
}

/// Every valid annotation reachable from `schema` through `properties` alone.
fn annotations(schema: &Value) -> Vec<Annotated> {
    let mut found = Vec::new();
    walk(schema, &mut Vec::new(), &mut found);
    // Case-insensitive uniqueness is a validity rule: a schema that names the
    // same header twice has no well-defined header to compare, so neither is
    // enforced.
    let mut names: Vec<String> = found.iter().map(|a| a.header.to_ascii_lowercase()).collect();
    names.sort();
    let duplicated: Vec<String> = names
        .windows(2)
        .filter(|pair| pair.first() == pair.get(1))
        .filter_map(|pair| pair.first().cloned())
        .collect();
    found.retain(|annotated| !duplicated.contains(&annotated.header.to_ascii_lowercase()));
    found
}

fn walk(schema: &Value, path: &mut Vec<String>, found: &mut Vec<Annotated>) {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    for (key, property) in properties {
        path.push(key.clone());
        if let Some(header) = property.get("x-mcp-header").and_then(Value::as_str) {
            let kind = match property.get("type").and_then(Value::as_str) {
                Some("string") => Some(Kind::String),
                Some("integer") => Some(Kind::Integer),
                Some("boolean") => Some(Kind::Boolean),
                _ => None,
            };
            if let Some(kind) = kind
                && is_token(header)
            {
                found.push(Annotated {
                    path: path.clone(),
                    header: header.to_owned(),
                    kind,
                });
            }
        }
        walk(property, path, found);
        path.pop();
    }
}

/// Decodes a header value per the sentinel rule, or `None` if it is invalid.
pub fn decode(value: &str) -> Option<String> {
    let Some(encoded) = value
        .strip_prefix("=?base64?")
        .and_then(|rest| rest.strip_suffix("?="))
    else {
        return Some(value.to_owned());
    };
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    String::from_utf8(bytes).ok()
}

/// Whether a decoded header value agrees with the body's value.
fn agrees(kind: Kind, header: &str, body: &Value) -> bool {
    match kind {
        Kind::String => body.as_str() == Some(header),
        Kind::Boolean => body.as_bool().map(|flag| if flag { "true" } else { "false" }) == Some(header),
        Kind::Integer => {
            let parsed = header.trim().parse::<f64>().ok();
            let body = body.as_f64();
            matches!((parsed, body), (Some(left), Some(right)) if (left - right).abs() < f64::EPSILON)
        }
    }
}

/// Checks every annotated argument against the `Mcp-Param-*` headers.
///
/// `headers` holds every `mcp-param-*` header, names lowercased. Returns the
/// offending header name on the first disagreement.
pub fn check(schema: &Value, arguments: &Value, headers: &[(String, String)]) -> Result<(), String> {
    for annotated in annotations(schema) {
        let mut value = Some(arguments);
        for key in &annotated.path {
            value = value.and_then(|current| current.get(key));
        }
        let name = format!("Mcp-Param-{}", annotated.header);
        let wanted = name.to_ascii_lowercase();
        let sent = headers
            .iter()
            .find(|(header, _)| *header == wanted)
            .map(|(_, value)| value.as_str());
        match (value.filter(|value| !value.is_null()), sent) {
            (None, _) => {}
            (Some(_), None) => return Err(name),
            (Some(body), Some(raw)) => {
                let decoded = decode(raw).ok_or_else(|| name.clone())?;
                if !agrees(annotated.kind, &decoded, body) {
                    return Err(name);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::check;

    fn schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "region": { "type": "string", "x-mcp-header": "Region" },
                "limit": { "type": "integer", "x-mcp-header": "Limit" },
                "nested": { "type": "object", "properties": {
                    "flag": { "type": "boolean", "x-mcp-header": "Flag" }
                }},
                "listed": { "type": "array", "items": { "type": "string", "x-mcp-header": "Item" } },
                "ratio": { "type": "number", "x-mcp-header": "Ratio" }
            }
        })
    }

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn matching_headers_pass() {
        let arguments = json!({ "region": "us-west1", "limit": 42, "nested": { "flag": true } });
        let sent = headers(&[
            ("Mcp-Param-Region", "us-west1"),
            ("Mcp-Param-Limit", "42.0"),
            ("Mcp-Param-Flag", "true"),
        ]);
        assert_eq!(check(&schema(), &arguments, &sent), Ok(()));
    }

    #[test]
    fn a_value_in_the_body_without_its_header_is_refused() {
        let arguments = json!({ "region": "us-west1" });
        assert!(check(&schema(), &arguments, &[]).is_err());
    }

    #[test]
    fn an_absent_or_null_value_expects_no_header() {
        assert_eq!(check(&schema(), &json!({ "region": null }), &[]), Ok(()));
        assert_eq!(check(&schema(), &json!({}), &[]), Ok(()));
    }

    #[test]
    fn base64_is_decoded_and_must_be_valid() {
        let arguments = json!({ "region": "Hello" });
        assert_eq!(
            check(&schema(), &arguments, &headers(&[("Mcp-Param-Region", "=?base64?SGVsbG8=?=")])),
            Ok(())
        );
        assert!(check(&schema(), &arguments, &headers(&[("Mcp-Param-Region", "=?base64?SGVsbG8?=")])).is_err());
        assert!(check(&schema(), &arguments, &headers(&[("Mcp-Param-Region", "=?base64?SGVs!!!bG8=?=")])).is_err());
    }

    #[test]
    fn a_value_without_the_exact_wrapper_is_literal() {
        let arguments = json!({ "region": "=?base64?SGVsbG8=" });
        assert_eq!(
            check(&schema(), &arguments, &headers(&[("Mcp-Param-Region", "=?base64?SGVsbG8=")])),
            Ok(())
        );
    }

    #[test]
    fn annotations_behind_items_or_on_numbers_are_not_enforced() {
        let arguments = json!({ "listed": ["a"], "ratio": 0.5 });
        assert_eq!(check(&schema(), &arguments, &[]), Ok(()));
    }

    #[test]
    fn invalid_annotations_are_detected() {
        assert!(super::has_invalid_annotation(&schema()), "items and number annotations are invalid");
        let clean = json!({ "type": "object", "properties": {
            "region": { "type": "string", "x-mcp-header": "Region" }
        }});
        assert!(!super::has_invalid_annotation(&clean));
        let duplicated = json!({ "type": "object", "properties": {
            "a": { "type": "string", "x-mcp-header": "Same" },
            "b": { "type": "string", "x-mcp-header": "same" }
        }});
        assert!(super::has_invalid_annotation(&duplicated));
    }

    #[test]
    fn the_encoding_examples_from_the_specification() {
        assert_eq!(super::encode("us-west1"), "us-west1");
        assert_eq!(super::encode("Hello, 世界"), "=?base64?SGVsbG8sIOS4lueVjA==?=");
        assert_eq!(super::encode(" padded "), "=?base64?IHBhZGRlZCA=?=");
        assert_eq!(super::encode("line1\nline2"), "=?base64?bGluZTEKbGluZTI=?=");
        assert_eq!(super::encode("=?base64?literal?="), "=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?=");
    }

    #[test]
    fn a_client_mirrors_what_the_server_checks() {
        let arguments = json!({ "region": "Hello, 世界", "limit": 7, "nested": { "flag": false } });
        let sent: Vec<(String, String)> = super::headers_for(&schema(), &arguments)
            .into_iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value))
            .collect();
        assert_eq!(sent.len(), 3);
        assert_eq!(check(&schema(), &arguments, &sent), Ok(()));
    }

    #[test]
    fn a_mismatch_is_refused() {
        let arguments = json!({ "region": "us-west1" });
        assert!(check(&schema(), &arguments, &headers(&[("Mcp-Param-Region", "eu-west1")])).is_err());
    }
}
