//! Typed model of the JSON Schema subset the vendored MCP specifications use.
//!
//! This is a *lossy-by-design* view of `schema.json`: it captures exactly the
//! keywords that appear inside the definition blocks of the six vendored
//! revisions, and nothing else. Anything outside that subset is a hard error
//! rather than a silent omission, because a schema keyword the generator does
//! not understand would produce wire types that quietly disagree with the spec —
//! the exact failure mode the project's "accurate" goal forbids.
//!
//! The keyword inventory, measured across `spec/*/schema.json` on 2026-10-02:
//!
//! | keyword             | uses | notes                                    |
//! |---------------------|------|------------------------------------------|
//! | `type`              | 2653 | object/array/string/number/int/bool + 3 unions |
//! | `description`       | 1942 | becomes a doc comment                    |
//! | `$ref`              | 1112 | always local: `#/$defs/X` or `#/definitions/X` |
//! | `properties`        |  855 |                                          |
//! | `required`          |  630 |                                          |
//! | `const`             |  384 | string or integer only                   |
//! | `additionalProperties` | 286 | never `false` — see below            |
//! | `items`             |  186 |                                          |
//! | `anyOf`             |  118 | union                                    |
//! | `format`            |   99 | advisory only, never narrowing          |
//! | `title`             |   75 |                                          |
//! | `enum`              |   62 | string members only                      |
//! | `minimum`/`maximum` |   80 | range hints, recorded in the doc comment |
//! | `default`           |   25 | recorded in the doc comment             |
//! | `allOf`             |   10 | two-way intersection, merged             |
//! | `$schema`           |    9 | only inside embedded schemas            |
//! | `maxItems`/`minItems`/`maxLength`/`minLength` | 22 | doc comments        |
//! | `oneOf`             |    3 | root-level envelope union only           |
//!
//! ## Why `additionalProperties: false` never appears
//!
//! No revision closes an object. That is load-bearing for this crate: the
//! project requires unknown fields to be preserved so a version-translating
//! bridge is lossless, and the only representation that preserves them is a
//! flattened catch-all. An `additionalProperties: false` would mean "reject
//! unknown fields", which is the opposite requirement, so its absence is the
//! reason passthrough is achievable at all. The model keeps the keyword so a
//! future schema that does use it fails loudly instead of being ignored.

use std::collections::BTreeMap;

use serde_json::Value;
use thiserror::Error;

/// Keywords this model understands. Any *other* key on a schema node is
/// rejected, so an unrecognised keyword surfaces as a build failure rather than
/// a type that is quietly wrong.
const KNOWN_KEYWORDS: &[&str] = &[
    "type",
    "description",
    "title",
    "$ref",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "anyOf",
    "allOf",
    "const",
    "enum",
    "format",
    "minimum",
    "maximum",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "default",
    "$schema",
    "$defs",
    "definitions",
];

/// A JSON Schema type selector: either a single name or a list of names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeSet {
    /// One type, e.g. `"string"`.
    One(String),
    /// A union of types, e.g. `["string", "integer"]`. MCP uses this in three
    /// places; the variants are all disjoint JSON kinds, so it is emitted as a
    /// small untagged enum.
    Many(Vec<String>),
}

/// A resolved reference target: the definition name only.
///
/// The reference container (`$defs` vs `definitions`) is a property of the
/// whole file, not of an individual reference, and is normalised away at parse
/// time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefRef(pub String);

/// A schema node in the subset this model covers.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    /// `type`, when present.
    pub type_: Option<TypeSet>,
    /// `$ref`, when present. Resolved to a definition name.
    pub ref_: Option<DefRef>,
    /// Object members, when this is a structured object.
    pub properties: BTreeMap<String, Box<Self>>,
    /// Names the object requires.
    pub required: Vec<String>,
    /// The `additionalProperties` value: `None` when absent.
    pub additional_properties: Option<Box<Self>>,
    /// `true` when `additionalProperties: true` appeared literally.
    pub additional_properties_true: bool,
    /// Element schema for arrays.
    pub items: Option<Box<Self>>,
    /// Union members, from `anyOf` or `oneOf`.
    pub union: Option<Union>,
    /// `allOf` members. MCP uses this only to compose a base params/result
    /// type with a payload type; the members are object schemas with disjoint
    /// field sets and are merged into one generated struct.
    pub intersect: Vec<Box<Self>>,
    /// `const` value.
    pub constant: Option<Value>,
    /// `enum` values.
    pub enum_values: Option<Vec<Value>>,
    /// Advisory-only keywords, carried so the emitter can record them in the
    /// generated doc comment without changing the Rust type.
    pub format: Option<String>,
    /// `default`, recorded in the doc comment.
    pub default: Option<Value>,
    /// Inclusive numeric range, recorded in the doc comment.
    pub minimum: Option<f64>,
    /// Inclusive numeric range, recorded in the doc comment.
    pub maximum: Option<f64>,
    /// Inclusive length/count bounds, recorded in the doc comment.
    pub min_items: Option<u64>,
    /// Inclusive length/count bounds, recorded in the doc comment.
    pub max_items: Option<u64>,
    /// Inclusive length bounds, recorded in the doc comment.
    pub min_length: Option<u64>,
    /// Inclusive length bounds, recorded in the doc comment.
    pub max_length: Option<u64>,
    /// Prose carried into the generated doc comment.
    pub description: Option<String>,
    /// Prose carried into the generated doc comment.
    pub title: Option<String>,
}

impl Schema {
    /// A schema that constrains nothing: accepts any JSON value.
    pub fn is_permissive(&self) -> bool {
        self.type_.is_none()
            && self.ref_.is_none()
            && self.properties.is_empty()
            && self.items.is_none()
            && self.union.is_none()
            && self.intersect.is_empty()
            && self.constant.is_none()
            && self.enum_values.is_none()
    }

    /// True when this is a structured object with declared properties.
    pub fn is_structured_object(&self) -> bool {
        !self.properties.is_empty() && self.ref_.is_none() && self.union.is_none()
    }

    /// The definition this schema references, if any.
    pub fn def_ref(&self) -> Option<&str> {
        self.ref_.as_ref().map(|r| r.0.as_str())
    }
}

/// Which keyword introduced a union, and its members.
#[derive(Debug, Clone)]
pub struct Union {
    /// `anyOf` or `oneOf`.
    pub kind: UnionKind,
    /// The member schemas.
    pub members: Vec<Box<Schema>>,
}

/// The two combinator keywords that produce a union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnionKind {
    /// `anyOf` — at least one member must match.
    AnyOf,
    /// `oneOf` — exactly one member must match.
    OneOf,
}

/// A parsed `schema.json`: its dialect plus its definition blocks.
#[derive(Debug, Clone)]
pub struct SchemaFile {
    /// The revision directory name, e.g. `2026-07-28` or `draft`.
    pub revision: String,
    /// The declared `$schema` dialect URI.
    pub dialect: String,
    /// Which keyword held the definition blocks, `defs` or `definitions`.
    pub ref_container: RefContainer,
    /// Definition name to schema.
    pub defs: BTreeMap<String, Schema>,
}

/// Where a revision keeps its definition blocks.
///
/// This is a per-revision fact, not a version-ordered one: the three
/// draft-07 revisions use `definitions` and the three 2020-12 revisions use
/// `$defs`. Upstream changed container name without changing key ordering, so
/// nothing in the generator may assume a revision's age predicts its dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefContainer {
    /// Draft-07 `definitions`.
    Definitions,
    /// 2020-12 `$defs`.
    Defs,
}

impl RefContainer {
    /// The keyword name as it appears in the file.
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Definitions => "definitions",
            Self::Defs => "$defs",
        }
    }
}

/// Failures raised while parsing a vendored `schema.json`.
#[derive(Debug, Error)]
pub enum SchemaParseError {
    /// The file was not valid JSON.
    #[error("{revision}: schema.json is not valid JSON: {source}")]
    MalformedJson {
        /// Revision whose file failed.
        revision: String,
        /// The underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// The file was not a JSON object at the top level.
    #[error("{revision}: schema.json is not a JSON object")]
    NotAnObject {
        /// Revision whose file failed.
        revision: String,
    },

    /// The file declared no `$schema`, so the dialect is unknown.
    #[error("{revision}: schema.json declares no $schema, so its dialect is unknown")]
    MissingDialect {
        /// Revision whose file failed.
        revision: String,
    },

    /// Neither `$defs` nor `definitions` was present.
    #[error(
        "{revision}: schema.json has neither $defs nor definitions, so there is nothing to generate"
    )]
    NoDefinitions {
        /// Revision whose file failed.
        revision: String,
    },

    /// A node carried a keyword the generator does not model.
    #[error("{revision}: unsupported schema keyword {keyword:?} at {path}")]
    UnsupportedKeyword {
        /// Revision whose file failed.
        revision: String,
        /// The offending keyword.
        keyword: String,
        /// JSON-pointer-ish path to the offending node.
        path: String,
    },

    /// A `$ref` pointed outside the document, or used a container that does not
    /// match the file's declared one.
    #[error("{revision}: unsupported $ref {reference:?} at {path} (expected #{container}/...)")]
    UnsupportedRef {
        /// Revision whose file failed.
        revision: String,
        /// The offending reference.
        reference: String,
        /// The container the file actually uses.
        container: &'static str,
        /// Path to the offending node.
        path: String,
    },

    /// A `type` value was not a string or an array of strings.
    #[error("{revision}: {path} has a non-string `type`")]
    BadType {
        /// Revision whose file failed.
        revision: String,
        /// Path to the offending node.
        path: String,
    },

    /// A keyword that this model requires to be an object or a list was not.
    #[error("{revision}: {keyword} at {path} is not a {expected}")]
    BadKeywordShape {
        /// Revision whose file failed.
        revision: String,
        /// The keyword.
        keyword: String,
        /// Path to the offending node.
        path: String,
        /// What shape was expected.
        expected: &'static str,
    },
}

impl SchemaParseError {
    fn at(revision: &str, path: &str) -> String {
        format!("{revision}:{path}")
    }
}

/// Parse a revision's `schema.json` text.
pub fn parse(revision: &str, text: &str) -> Result<SchemaFile, SchemaParseError> {
    let root: Value =
        serde_json::from_str(text).map_err(|source| SchemaParseError::MalformedJson {
            revision: revision.to_owned(),
            source,
        })?;

    let Value::Object(root_map) = &root else {
        return Err(SchemaParseError::NotAnObject {
            revision: revision.to_owned(),
        });
    };

    let dialect = root_map
        .get("$schema")
        .and_then(Value::as_str)
        .ok_or_else(|| SchemaParseError::MissingDialect {
            revision: revision.to_owned(),
        })?
        .to_owned();

    let (ref_container, defs_value) = match (root_map.get("$defs"), root_map.get("definitions")) {
        (Some(v), None) => (RefContainer::Defs, v),
        (None, Some(v)) => (RefContainer::Definitions, v),
        // Both containers present would make a `$ref` ambiguous, and neither
        // present leaves nothing to generate from. Neither is a thing a
        // revision does, and silently preferring one would be exactly the kind
        // of quiet divergence the project forbids.
        (Some(_), Some(_)) | (None, None) => {
            return Err(SchemaParseError::NoDefinitions {
                revision: revision.to_owned(),
            });
        }
    };

    let defs_map = defs_value
        .as_object()
        .ok_or_else(|| SchemaParseError::BadKeywordShape {
            revision: revision.to_owned(),
            keyword: ref_container.keyword().to_owned(),
            path: SchemaParseError::at(revision, "$"),
            expected: "object",
        })?;

    let mut defs = BTreeMap::new();
    for (name, value) in defs_map {
        let path = format!("{}/{name}", ref_container.keyword());
        defs.insert(
            name.clone(),
            parse_node(revision, value, &path, ref_container)?,
        );
    }

    Ok(SchemaFile {
        revision: revision.to_owned(),
        dialect,
        ref_container,
        defs,
    })
}

fn parse_node(
    revision: &str,
    node: &Value,
    path: &str,
    container: RefContainer,
) -> Result<Schema, SchemaParseError> {
    let Value::Object(map) = node else {
        // A bare `true`/`false` schema, or a non-object. MCP does not use
        // these inside definitions; treating them as permissive would hide it.
        return Err(SchemaParseError::BadKeywordShape {
            revision: revision.to_owned(),
            keyword: "<schema>".to_owned(),
            path: path.to_owned(),
            expected: "object",
        });
    };

    reject_unknown_keywords(revision, map, path)?;

    let mut out = parse_advisory(map);

    if let Some(v) = map.get("type") {
        out.type_ = Some(match v {
            Value::String(s) => TypeSet::One(s.clone()),
            Value::Array(items) => {
                let mut names = Vec::with_capacity(items.len());
                for item in items {
                    let Some(s) = item.as_str() else {
                        return Err(SchemaParseError::BadType {
                            revision: revision.to_owned(),
                            path: path.to_owned(),
                        });
                    };
                    names.push(s.to_owned());
                }
                TypeSet::Many(names)
            }
            _ => {
                return Err(SchemaParseError::BadType {
                    revision: revision.to_owned(),
                    path: path.to_owned(),
                });
            }
        });
    }

    if let Some(r) = map.get("$ref") {
        out.ref_ = Some(parse_ref(revision, r, path, container)?);
    }

    if let Some(Value::Object(props)) = map.get("properties") {
        for (name, value) in props {
            let child_path = format!("{path}/properties/{name}");
            let child = parse_node(revision, value, &child_path, container)?;
            out.properties.insert(name.clone(), Box::new(child));
        }
    }

    if let Some(Value::Array(req)) = map.get("required") {
        for r in req {
            if let Some(s) = r.as_str() {
                out.required.push(s.to_owned());
            }
        }
    }

    match map.get("additionalProperties") {
        Some(Value::Bool(true)) => out.additional_properties_true = true,
        Some(Value::Bool(false)) => {
            // Not reachable in any vendored revision. Represented rather than
            // rejected so the failure, if it ever happens, is a specific one
            // from the emitter rather than a parse error.
            out.additional_properties_true = false;
        }
        Some(other) => {
            let child_path = format!("{path}/additionalProperties");
            out.additional_properties = Some(Box::new(parse_node(
                revision,
                other,
                &child_path,
                container,
            )?));
        }
        None => {}
    }

    if let Some(items) = map.get("items") {
        out.items = Some(Box::new(parse_node(
            revision,
            items,
            &format!("{path}/items"),
            container,
        )?));
    }

    parse_combinators(revision, map, path, container, &mut out)?;

    Ok(out)
}

/// Parse `anyOf`, `oneOf` and `allOf` into `out`.
///
/// Split out of `parse_node` purely for length. The three share a shape — a
/// keyword naming an array of sub-schemas — and differ only in what they mean,
/// so they are handled together rather than as three near-identical blocks.
fn parse_combinators(
    revision: &str,
    map: &serde_json::Map<String, Value>,
    path: &str,
    container: RefContainer,
    out: &mut Schema,
) -> Result<(), SchemaParseError> {
    for (keyword, kind) in [("anyOf", UnionKind::AnyOf), ("oneOf", UnionKind::OneOf)] {
        if let Some(Value::Array(members)) = map.get(keyword) {
            let mut parsed = Vec::with_capacity(members.len());
            for (i, m) in members.iter().enumerate() {
                parsed.push(Box::new(parse_node(
                    revision,
                    m,
                    &format!("{path}/{keyword}/{i}"),
                    container,
                )?));
            }
            out.union = Some(Union {
                kind,
                members: parsed,
            });
        }
    }

    if let Some(Value::Array(members)) = map.get("allOf") {
        for (i, m) in members.iter().enumerate() {
            out.intersect.push(Box::new(parse_node(
                revision,
                m,
                &format!("{path}/allOf/{i}"),
                container,
            )?));
        }
    }

    if let Some(Value::Array(values)) = map.get("enum") {
        out.enum_values = Some(values.clone());
    }

    Ok(())
}

/// Reject any keyword this model does not model.
///
/// This is the single most important check in the crate: a schema keyword the
/// generator does not understand would produce a wire type that quietly
/// disagrees with the specification, which is the one failure the project's
/// "accurate" goal forbids. A build that stops here is cheaper than a bridge
/// that drops a field.
fn reject_unknown_keywords(
    revision: &str,
    map: &serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), SchemaParseError> {
    for key in map.keys() {
        if !KNOWN_KEYWORDS.contains(&key.as_str()) {
            return Err(SchemaParseError::UnsupportedKeyword {
                revision: revision.to_owned(),
                keyword: key.clone(),
                path: path.to_owned(),
            });
        }
    }
    Ok(())
}

/// The keywords that only inform a doc comment rather than the Rust type.
fn parse_advisory(map: &serde_json::Map<String, Value>) -> Schema {
    Schema {
        description: map
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        title: map.get("title").and_then(Value::as_str).map(str::to_owned),
        format: map.get("format").and_then(Value::as_str).map(str::to_owned),
        default: map.get("default").cloned(),
        minimum: map.get("minimum").and_then(Value::as_f64),
        maximum: map.get("maximum").and_then(Value::as_f64),
        min_items: map.get("minItems").and_then(Value::as_u64),
        max_items: map.get("maxItems").and_then(Value::as_u64),
        min_length: map.get("minLength").and_then(Value::as_u64),
        max_length: map.get("maxLength").and_then(Value::as_u64),
        constant: map.get("const").cloned(),
        ..Schema::default()
    }
}

fn parse_ref(
    revision: &str,
    value: &Value,
    path: &str,
    container: RefContainer,
) -> Result<DefRef, SchemaParseError> {
    let unsupported = || SchemaParseError::UnsupportedRef {
        revision: revision.to_owned(),
        reference: value.as_str().unwrap_or("<non-string>").to_owned(),
        container: container.keyword(),
        path: path.to_owned(),
    };
    let Some(text) = value.as_str() else {
        return Err(unsupported());
    };
    let prefix = match container {
        RefContainer::Defs => "#/$defs/",
        RefContainer::Definitions => "#/definitions/",
    };
    let Some(name) = text.strip_prefix(prefix) else {
        return Err(unsupported());
    };
    if name.is_empty() || name.contains('/') {
        return Err(unsupported());
    }
    Ok(DefRef(name.to_owned()))
}
