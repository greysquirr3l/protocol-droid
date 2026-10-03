//! Turning JSON member names into legal, idiomatic Rust identifiers.
//!
//! Every name in a vendored `schema.json` is legal JSON and mostly legal Rust,
//! but three families are not, and each needs a mechanical, reversible answer:
//!
//! - **Rust keywords.** `Tool.inputSchema` is fine, but `SchemaType.type` and
//!   `Prompt.enum` are not. Keywords get a trailing underscore so the generated
//!   field is `type_`, with `#[serde(rename = "type")]` restoring the wire name.
//! - **Namespaced keys.** `RequestMetaObject` declares
//!   `io.modelcontextprotocol/clientInfo`. The dots and slash are legal JSON and
//!   illegal Rust; both become underscores.
//! - **Hyphenated and underscored values.** `LoggingLevel` includes `"date-time"`,
//!   and `TaskStatus` includes `"input_required"`. Both become `DateTime` and
//!   `InputRequired` as enum variants.
//!
//! Nothing here inspects the schema; these are pure string functions so they can
//! be tested exhaustively without a fixture.

/// Reserved words in Rust 2024 that cannot appear as a bare identifier.
///
/// `union` is included: it is not reserved in 2024, but it is a plausible MCP
/// member name and reserving it costs nothing.
const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "box", "break", "const", "continue", "crate", "do", "dyn", "else",
    "enum", "extern", "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "macro",
    "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return", "self", "static",
    "struct", "super", "trait", "true", "try", "type", "typeof", "union", "unsafe", "unsized",
    "use", "virtual", "where", "while", "yield",
];

/// Whether `name` is a word Rust will not accept as a bare identifier.
#[must_use]
pub fn is_rust_keyword(name: &str) -> bool {
    RUST_KEYWORDS.contains(&name)
}

/// Split a JSON name into lowercase words.
///
/// Handles `camelCase`, `PascalCase`, `kebab-case`, `snake_case`, dotted
/// reverse-DNS prefixes, and any mixture, without needing the name to be
/// consistent in the first place — upstream is not.
///
/// # Examples
///
/// ```
/// use protocol_droid_schema::adapters::primary::generator::naming::words;
///
/// assert_eq!(words("inputSchema"), ["input", "schema"]);
/// assert_eq!(words("JSONRPCRequest"), ["jsonrpc", "request"]);
/// assert_eq!(words("io.modelcontextprotocol/clientInfo"), ["io", "modelcontextprotocol", "client", "info"]);
/// assert_eq!(words("_meta"), ["meta"]);
/// ```
#[must_use]
pub fn words(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for chunk in name.split(|c: char| !c.is_ascii_alphanumeric()) {
        if chunk.is_empty() {
            continue;
        }
        // A leading underscore carries no information in these names: `_meta` is
        // the meta object, not a private field called `meta`. Dropping it keeps
        // the snake and Pascal forms predictable instead of producing `_meta`.
        let chunk = chunk.trim_start_matches('_');
        if chunk.is_empty() {
            continue;
        }
        push_camel_words(chunk, &mut out);
    }
    out
}

/// Break a single alphanumeric chunk on camel-case and acronym boundaries.
fn push_camel_words(chunk: &str, out: &mut Vec<String>) {
    let mut current = String::new();
    let mut prev = None;
    let mut chars = chunk.chars().peekable();
    while let Some(cur) = chars.next() {
        if let Some(prev_char) = prev {
            let boundary = (prev_char.is_ascii_lowercase() && cur.is_ascii_uppercase())
                // `JSONRPCRequest`: split before the last capital of a run, so
                // the acronym stays whole and only the following word detaches.
                || (prev_char.is_ascii_uppercase()
                    && cur.is_ascii_uppercase()
                    && chars.peek().is_some_and(char::is_ascii_lowercase));
            if boundary && !current.is_empty() {
                out.push(std::mem::take(&mut current).to_ascii_lowercase());
            }
        }
        current.push(cur);
        prev = Some(cur);
    }
    if !current.is_empty() {
        out.push(current.to_ascii_lowercase());
    }
}

/// Whether the character after `at` is lowercase, making `at` the end of a run
/// of capitals rather than its start.

/// A Rust `snake_case` identifier for a JSON member name.
///
/// Keywords gain a trailing underscore; an empty or fully non-alphanumeric
/// name becomes `_` rather than an empty identifier.
///
/// # Examples
///
/// ```
/// use protocol_droid_schema::adapters::primary::generator::naming::field_ident;
///
/// assert_eq!(field_ident("isError"), "is_error");
/// assert_eq!(field_ident("type"), "type_");
/// assert_eq!(field_ident("io.modelcontextprotocol/clientInfo"), "io_modelcontextprotocol_client_info");
/// ```
#[must_use]
pub fn field_ident(name: &str) -> String {
    finish(words(name).join("_"), true)
}

/// A Rust `PascalCase` identifier for a JSON name.
///
/// Used for enum variants and for the generated types that stand in for an
/// inline schema, which has no name of its own in the source document.
///
/// # Examples
///
/// ```
/// use protocol_droid_schema::adapters::primary::generator::naming::variant_ident;
///
/// assert_eq!(variant_ident("date-time"), "DateTime");
/// assert_eq!(variant_ident("input_required"), "InputRequired");
/// assert_eq!(variant_ident("allServers"), "AllServers");
/// ```
#[must_use]
pub fn variant_ident(name: &str) -> String {
    let joined: String = words(name)
        .into_iter()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    finish(joined, false)
}

/// A Rust module identifier for a revision directory name.
///
/// `2024-11-05` is not a legal identifier, and `2024_11_05` is not legal as the
/// first segment of a type path, so the leading digit is escaped and the
/// trailing one is dropped: `draft` has no version number to lose, and the
/// other five all keep theirs.
///
/// # Examples
///
/// ```
/// use protocol_droid_schema::adapters::primary::generator::naming::module_ident;
///
/// assert_eq!(module_ident("2024-11-05"), "v2024_11_05");
/// assert_eq!(module_ident("draft"), "v_draft");
/// ```
#[must_use]
pub fn module_ident(revision: &str) -> String {
    let joined = words(revision).join("_");
    if joined.is_empty() {
        return "v".to_owned();
    }
    format!("v{joined}")
}

/// Apply the legality rules shared by both casings.
fn finish(joined: String, allow_keyword_suffix: bool) -> String {
    if joined.is_empty() {
        return "_".to_owned();
    }
    if joined.starts_with(|c: char| c.is_ascii_digit()) {
        return format!("_{joined}");
    }
    if allow_keyword_suffix && is_rust_keyword(&joined) {
        return format!("{joined}_");
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::{field_ident, is_rust_keyword, module_ident, variant_ident, words};

    #[test]
    fn splits_every_casing_upstream_uses() {
        assert_eq!(words("inputSchema"), ["input", "schema"]);
        assert_eq!(words("JSONRPCRequest"), ["jsonrpc", "request"]);
        assert_eq!(words("isError"), ["is", "error"]);
        assert_eq!(words("nextCursor"), ["next", "cursor"]);
        assert_eq!(words("date-time"), ["date", "time"]);
        assert_eq!(words("input_required"), ["input", "required"]);
        assert_eq!(words("allServers"), ["all", "servers"]);
        assert_eq!(
            words("io.modelcontextprotocol/clientInfo"),
            ["io", "modelcontextprotocol", "client", "info"]
        );
        assert_eq!(words("_meta"), ["meta"]);
        assert_eq!(words("jsonrpc"), ["jsonrpc"]);
    }

    #[test]
    fn an_all_capital_name_stays_one_word() {
        assert_eq!(words("MCP"), ["mcp"]);
        assert_eq!(words("URI"), ["uri"]);
    }

    #[test]
    fn empty_and_symbol_only_names_are_still_identifiers() {
        assert_eq!(field_ident(""), "_");
        assert_eq!(field_ident("---"), "_");
        assert_eq!(variant_ident(""), "_");
    }

    #[test]
    fn a_leading_digit_is_escaped() {
        assert_eq!(field_ident("2fa"), "_2fa");
    }

    #[test]
    fn keywords_get_a_trailing_underscore() {
        assert!(is_rust_keyword("type"));
        assert!(is_rust_keyword("ref"));
        assert!(!is_rust_keyword("tools"));
        assert_eq!(field_ident("type"), "type_");
        assert_eq!(field_ident("ref"), "ref_");
        assert_eq!(field_ident("enum"), "enum_");
        assert_eq!(field_ident("tool"), "tool");
    }

    #[test]
    fn revisions_map_to_legal_modules() {
        assert_eq!(module_ident("2024-11-05"), "v2024_11_05");
        assert_eq!(module_ident("2026-07-28"), "v2026_07_28");
        assert_eq!(module_ident("draft"), "v_draft");
    }

    #[test]
    fn the_mcp_property_set_converts_without_collisions() {
        // The 136 distinct property names across all six vendored revisions.
        let names = [
            "_meta",
            "cacheScope",
            "clientInfo",
            "costPriority",
            "createdAt",
            "destructiveHint",
            "enum",
            "enumNames",
            "idempotentHint",
            "includeContext",
            "inputRequests",
            "inputResponses",
            "inputSchema",
            "isError",
            "io.modelcontextprotocol/clientCapabilities",
            "io.modelcontextprotocol/clientInfo",
            "io.modelcontextprotocol/logLevel",
            "io.modelcontextprotocol/protocolVersion",
            "io.modelcontextprotocol/serverInfo",
            "io.modelcontextprotocol/subscriptionId",
            "lastModified",
            "maxItems",
            "maxLength",
            "maxTokens",
            "mimeType",
            "nextCursor",
            "oneOf",
            "outputSchema",
            "pollInterval",
            "progressToken",
            "promptsListChanged",
            "readOnlyHint",
            "ref",
            "requestId",
            "resourceSubscriptions",
            "resourcesListChanged",
            "rootUri",
            "stopSequences",
            "structuredContent",
            "supportedVersions",
            "systemPrompt",
            "taskSupport",
            "title",
            "toolChoice",
            "toolsListChanged",
            "ttlMs",
            "type",
            "uriTemplate",
            "websiteUrl",
        ];
        let mut seen = std::collections::BTreeMap::new();
        for name in names {
            let ident = field_ident(name);
            if let Some(previous) = seen.insert(ident.clone(), name) {
                panic!("{previous} and {name} both map to {ident}");
            }
        }
    }
}
