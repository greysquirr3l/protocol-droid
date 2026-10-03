//! Emits Rust source for one revision from its parsed [`SchemaFile`].
//!
//! # The one property that matters
//!
//! **Every generated object type carries a flattened catch-all.** Without it,
//! `serde` drops any JSON member the type does not name, and the
//! version-translating bridge silently loses data it never sees. That is the
//! failure this crate exists to prevent, and it is the reason the generator is
//! hand-built rather than delegated: the obvious off-the-shelf generators emit
//! no such field for a schema that omits `additionalProperties`, which is what
//! MCP does almost everywhere.
//!
//! The field is `#[serde(flatten)] extra: serde_json::Map<String,
//! serde_json::Value>`. It is never `Option` and never `skip_serializing_if`:
//! an absent catch-all and a present-but-empty one serialise identically, and
//! making it optional would only create a second state to forget to handle.
//!
//! # Naming
//!
//! Every revision's module exposes the *same* top-level type names, so the
//! codec layer can instantiate one generic implementation per revision. A name
//! that only exists in some revisions is a signal the schema gained a type
//! that revision does not have, and is emitted as a type alias to
//! `serde_json::Value` rather than omitted — absence would turn a schema
//! evolution into a compile error somewhere far from its cause.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

use super::schema_model::{Schema, SchemaFile, TypeSet, Union};

/// The Rust type a schema node lowers to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RustType {
    /// A reference to another generated type, by name.
    Named(String),
    /// `bool`.
    Bool,
    /// A signed integer.
    Int,
    /// A float.
    Float,
    /// An owned string. `Cow<'de, str>` cannot be used here: `serde::Deserialize`
    /// cannot be derived for a type with a `'de` lifetime parameter, because
    /// deserialisation has no borrow to tie that lifetime to. A `&'de str` would
    /// work for `Deserialize`, but then `Serialize` would need a matching borrow
    /// on the way out, which defeats the point of owning the wire types.
    Str,
    /// An arbitrary JSON value, passed through untouched.
    Json,
    /// `Vec<T>`.
    List(Box<Self>),
}

impl RustType {
    /// The Rust source text for this type.
    fn render(&self) -> String {
        match self {
            Self::Named(n) => n.clone(),
            Self::Bool => "bool".to_owned(),
            Self::Int => "i64".to_owned(),
            Self::Float => "f64".to_owned(),
            Self::Str => "String".to_owned(),
            Self::Json => "serde_json::Value".to_owned(),
            Self::List(inner) => format!("Vec<{}>", inner.render()),
        }
    }
}

/// The result of emitting one revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    /// The module source, ready to write to `wire/<module>.rs`.
    pub source: String,
    /// Definition names in this revision, sorted.
    pub type_names: Vec<String>,
    /// Names the schema references but does not define. A non-empty list means
    /// a dangling `$ref` survived parsing, which would become a Rust type that
    /// does not exist.
    pub dangling_refs: Vec<String>,
}

/// Write the module's provenance header and preamble.
///
/// Split out of [`emit`] for length only; the content is unchanged.
fn write_preamble(source: &mut String, file: &SchemaFile) {
    let _ = writeln!(
        source,
        "//! Wire types for MCP revision `{}`.",
        file.revision
    );
    let _ = writeln!(source, "//!");
    let _ = writeln!(
        source,
        "//! GENERATED — do not edit. Produced by `generator::emit`"
    );
    let _ = writeln!(
        source,
        "//! from the vendored `spec/{}/schema.json`.",
        file.revision
    );
    let _ = writeln!(source, "//!");
    let _ = writeln!(
        source,
        "//! Dialect: `{}`, reference container `{}`, {} definitions.",
        file.dialect,
        file.ref_container.keyword(),
        file.defs.len()
    );
    let _ = writeln!(source, "//!");
    let _ = writeln!(
        source,
        "//! Every object type carries a flattened catch-all so unknown members"
    );
    let _ = writeln!(
        source,
        "//! survive a decode/encode round trip, which is what makes a"
    );
    let _ = writeln!(
        source,
        "//! version-translating bridge lossless. See the generator's module docs."
    );
    let _ = writeln!(source);
}

/// Emit the module source for `file`.
///
/// This is a pure function of the model: it reads no files and consults no
/// globals, so the generated output can be asserted on exactly.
pub fn emit(file: &SchemaFile) -> Generated {
    let mut source = String::with_capacity(64 * 1024);
    write_preamble(&mut source, file);

    // Lint allowances, not `expect`. An `expect` is itself an error when
    // unfulfilled, so a revision that happens not to trip one of these lints
    // would fail to build. An `allow` that turns out to be unnecessary is
    // silent, which is the behaviour wanted here: the set is a floor covering
    // the whole spec's vocabulary, not a per-revision claim.
    //
    // Each names a property of the *specification* that the generated code
    // faithfully reproduces, so fixing the lint would mean making the type
    // disagree with the schema — the one thing this crate must never do.
    //
    // - `derive_partial_eq_without_eq`: a schema that says `"type": "number"`
    //   produces an `f64` field, and `f64` is not `Eq`. Upstream permits `2.0`
    //   and `2` to be the same value.
    // - `large_enum_variant`: a union's alternatives differ in size because the
    //   specification's types do. Boxing would change the wire type's shape.
    // - `doc_*`: doc comments are copied verbatim from the vendored schema, so
    //   their wording, and any backticks or list indentation in it, is
    //   upstream's rather than this generator's.
    // - `struct_field_names` / `module_name_repetitions`: a field is named
    //   after the JSON member the specification names it by; renaming it would
    //   be a wire-format change.
    // - `use_self`: a recursive schema (`JSONValue` containing itself) is
    //   referenced by name from a *different* generated type — the union enum
    //   beside it. `Self` would name the enum, not the definition, so the
    //   full path is the only correct spelling here.
    for lint in [
        "clippy::derive_partial_eq_without_eq",
        "clippy::large_enum_variant",
        "clippy::doc_markdown",
        "clippy::doc_lazy_continuation",
        "clippy::struct_field_names",
        "clippy::use_self",
        "rustdoc::broken_intra_doc_links",
    ] {
        let _ = writeln!(source, "#![allow({lint})]");
    }
    let _ = writeln!(source);
    let _ = writeln!(source, "use serde::{{Deserialize, Serialize}};");
    let _ = writeln!(source);

    // Unions are collected before anything is emitted, because a property can
    // reference a union declared later in the file. Each one is keyed by the
    // definition that owns it, so two unions inside different definitions never
    // collide on a name.
    let mut pending: Vec<(String, Union, String)> = Vec::new();
    let mut multi_types: Vec<(String, String)> = Vec::new();
    for (name, schema) in &file.defs {
        collect_unions(schema, name, name, &mut pending);
        collect_multi_types(schema, &mut multi_types);
    }
    multi_types.sort();
    multi_types.dedup();
    pending.sort_by(|a, b| a.0.cmp(&b.0));
    pending.dedup_by(|a, b| a.0 == b.0);

    for (ident, union, doc_name) in &pending {
        let members: Vec<RustType> = union
            .members
            .iter()
            .enumerate()
            // The path extension matches `collect_unions`, which walks a
            // member as `path_m{i}`. Both must agree: a member reached through
            // an array reuses the same segment, so the name it resolves to is
            // the one the collector recorded.
            .map(|(i, m)| lower(m, &file.defs, &member_path(ident, i)))
            .collect();

        // A member that fixes its own `type` is a union that can be told apart
        // by its discriminant rather than by member order. See
        // `discriminants` for why that distinction is the whole point.
        let discriminants: Option<Vec<&str>> = union
            .members
            .iter()
            .map(|m| type_const(m, &file.defs))
            .collect::<Option<Vec<_>>>()
            .filter(|tags| {
                // Dispatch is only sound when the discriminant is *injective* --
                // when a tag identifies exactly one alternative. A union whose
                // members nest their own unions can have three members all
                // tagged `string`, and a `match` on that cannot tell them apart
                // however it is written. Those keep the untagged fallback, which
                // is right: the tag is not carrying the information the arms
                // differ by.
                let unique = tags.iter().collect::<std::collections::BTreeSet<_>>();
                unique.len() == tags.len()
            });

        match &discriminants {
            Some(tags) => {
                let _ = writeln!(
                    source,
                    "/// One of the alternatives the specification allows for `{doc_name}`.\n\
                     ///\n\
                     /// Dispatched on the `type` member. Every alternative fixes\n\
                     /// its own `type`, so the JSON *does* say which one was sent\n\
                     /// even though it is not wrapped — and matching on member order\n\
                     /// instead would be guessing. `ImageContent` and `AudioContent`\n\
                     /// are both `{{ data, mimeType }}`; tried in order, an audio\n\
                     /// block matches the image arm and is re-emitted with\n\
                     /// `\"type\": \"image\"`, silently and without an error.\n\
                     ///\n\
                     /// An unrecognised or absent `type` is a decode failure rather\n\
                     /// than a fall-through, so a malformed message cannot be quietly\n\
                     /// accepted as whichever alternative happens to parse."
                );
                // `Serialize` keeps `untagged`; only `Deserialize` is
                // hand-written. The attribute has to stay even though the
                // manual impl ignores it, or the enum encodes as
                // `{"Member0": {...}}` rather than as the member itself.
                let _ = writeln!(source, "#[derive(Debug, Clone, PartialEq, Serialize)]");
                let _ = writeln!(source, "#[serde(untagged)]");
                let _ = writeln!(source, "pub enum {ident} {{");
                for (i, member) in members.iter().enumerate() {
                    let _ = writeln!(source, "    /// Alternative {i}.", i = i + 1);
                    let _ = writeln!(source, "    Member{i}({ty}),", ty = member.render());
                }
                let _ = writeln!(source, "}}\n");
                let _ = writeln!(source, "impl<'de> serde::Deserialize<'de> for {ident} {{");
                // `Result` is fully qualified because the specification defines
                // a type of that name, and it is in scope in every generated
                // module: an unqualified `Result<Self, D::Error>` resolves to
                // the MCP one, which takes no generic arguments.
                let _ = writeln!(
                    source,
                    "    fn deserialize<D>(deserializer: D) -> ::core::result::Result<Self, D::Error>\n    \
                     where\n        D: serde::Deserializer<'de>,\n    {{"
                );
                let _ = writeln!(
                    source,
                    "        let value = serde_json::Value::deserialize(deserializer)?;\n\
                     \x20       let tag = value\n            .get(\"type\")\n            .and_then(serde_json::Value::as_str)\n            .ok_or_else(|| serde::de::Error::custom(\"a `{doc_name}` must carry a `type` string\"))?;\n\
                     \x20       match tag {{"
                );
                for (i, tag) in tags.iter().enumerate() {
                    let _ = writeln!(
                        source,
                        "            \"{tag}\" => serde_json::from_value(value)\n                \
                         .map(|inner| {ident}::Member{i}(inner))\n                \
                         .map_err(<D::Error as serde::de::Error>::custom),"
                    );
                }
                let listed = tags.join(", ");
                let _ = writeln!(
                    source,
                    "            other => Err(<D::Error as serde::de::Error>::custom(format!(\n                \
                     \"`{{other}}` is not a {doc_name}; expected one of {listed}\"))),"
                );
                let _ = writeln!(source, "        }}");
                let _ = writeln!(source, "    }}");
                let _ = writeln!(source, "}}\n");
            }
            None => {
                let _ = writeln!(
                    source,
                    "/// One of the alternatives the specification allows for `{doc_name}`.\n\
                     ///\n\
                     /// Emitted untagged: the alternatives are not told apart by a\n\
                     /// `type` member, so serde matches the first that round-trips and\n\
                     /// the ordering here is the schema's own."
                );
                let _ = writeln!(
                    source,
                    "#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]"
                );
                let _ = writeln!(source, "#[serde(untagged)]");
                let _ = writeln!(source, "pub enum {ident} {{");
                for (i, member) in members.iter().enumerate() {
                    let _ = writeln!(source, "    /// Alternative {i}.", i = i + 1);
                    let _ = writeln!(source, "    Member{i}({ty}),", ty = member.render());
                }
                let _ = writeln!(source, "}}\n");
            }
        }
    }

    let _ = writeln!(
        source,
        "// A `\"type\": [..]` selector names a set of JSON kinds, and a value that\n\
         // matches more than one of them cannot be a single Rust type. `Value`\n\
         // is the honest lowering: the codec layer, not the wire type, decides\n\
         // which kind it is. Each distinct set gets its own name so the\n\
         // specification's own vocabulary stays visible in the generated code."
    );
    for (ident, names) in multi_types {
        let _ = writeln!(
            source,
            "/// One of the JSON kinds the specification allows: {names}."
        );
        let _ = writeln!(source, "pub type {ident} = serde_json::Value;");
    }
    let _ = writeln!(source);

    let mut dangling = Vec::new();
    for (name, schema) in &file.defs {
        if let Some(missing) = collect_dangling(schema, &file.defs) {
            dangling.extend(missing);
        }
        let () = emit_def(&mut source, name, schema, &file.defs);
    }

    dangling.sort_unstable();
    dangling.dedup();

    Generated {
        source,
        type_names: file.defs.keys().cloned().collect(),
        dangling_refs: dangling,
    }
}

/// The `type` value a union member fixes for itself, if it fixes one.
///
/// The specification discriminates its content blocks with
/// `{"type": "image", "const": ...}`-shaped members, and the model already
/// carries that `const` — [`Schema::constant`]. Reading it back here is what
/// lets the union dispatch on the tag the peer actually sent.
///
/// A member that is not a reference, or whose target declares no `type`, or
/// whose `type` carries no string `const`, yields `None`; the caller then
/// falls back to matching on order, because a union with a member that fixes
/// no tag cannot be dispatched at all.
fn type_const<'a>(member: &'a Schema, defs: &'a BTreeMap<String, Schema>) -> Option<&'a str> {
    let target = member.def_ref()?;
    let property = defs.get(target)?.properties.get("type")?;
    property.constant.as_ref()?.as_str()
}

fn emit_def(out: &mut String, name: &str, schema: &Schema, defs: &BTreeMap<String, Schema>) {
    let ident = type_ident(name);

    // A definition is a struct when it declares properties, an enum for a bare
    // `enum`, and a type alias otherwise. MCP's definitions are overwhelmingly
    // the first case.
    if let Some(values) = &schema.enum_values {
        let _ = writeln!(
            out,
            "/// `{name}` — a closed set of string values.\n\
             #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]\n\
             pub enum {ident} {{"
        );
        for (i, value) in values.iter().enumerate() {
            if let Some(s) = value.as_str() {
                let _ = writeln!(out, "    /// `{s}`.");
                let _ = writeln!(out, "    #[serde(rename = \"{s}\")]");
                let _ = writeln!(out, "    {variant},", variant = variant_ident(s, i));
            }
        }
        let _ = writeln!(out, "}}\n");
        return;
    }

    if !schema.is_structured_object() && schema.intersect.is_empty() {
        let ty = lower(schema, defs, name);
        let _ = ty;
        let _ = writeln!(out, "/// `{name}` — see the specification for its shape.");
        let _ = writeln!(out, "pub type {ident} = {};\n", ty.render());
        return;
    }

    // `allOf` is a two-way intersection in MCP (an interface merging a base
    // object). Flattening it into one struct is what the schema means; a
    // generated `AllOf<Base, Derived>` would push the merge onto every caller.
    // `BTreeMap` keeps the merge deterministic and collapses the duplicates a
    // two-way intersection can introduce.
    let mut properties: BTreeMap<&str, &Schema> = BTreeMap::new();
    for part in &schema.intersect {
        for (name, prop) in &part.properties {
            properties.insert(name.as_str(), prop);
        }
    }
    for (name, prop) in &schema.properties {
        properties.insert(name.as_str(), prop);
    }
    let required: Vec<&str> = schema.required.iter().map(String::as_str).collect();

    if let Some(doc) = doc_lines(name, schema.description.as_deref(), schema.title.as_deref()) {
        let _ = write!(out, "{doc}");
    }
    // `Default` is only derivable when every field can have one. An enum
    // field — a closed string set, or a union — cannot, and deriving it anyway
    // is a compile error rather than a lint, so it is emitted conditionally
    // from the field types rather than unconditionally.
    let all_default = properties.values().all(|p| {
        let ty = lower(p, defs, name);
        matches!(ty, RustType::Bool | RustType::Int | RustType::Float | RustType::Str | RustType::Json)
            || matches!(&ty, RustType::List(inner) if matches!(**inner, RustType::Bool | RustType::Int | RustType::Float | RustType::Str | RustType::Json))
    });
    let derives = if all_default {
        "#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]"
    } else {
        "#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]"
    };
    let _ = writeln!(out, "{derives}");
    let _ = writeln!(out, "pub struct {ident} {{");

    for (prop_name, prop) in &properties {
        if let Some(doc) = doc_lines(
            prop_name,
            prop.description.as_deref(),
            prop.title.as_deref(),
        ) {
            let _ = write!(out, "{doc}");
        }
        let ty = lower(prop, defs, &format!("{name}_{prop_name}"));
        let field = field_ident(prop_name);
        if field != *prop_name {
            let _ = writeln!(out, "    #[serde(rename = \"{prop_name}\")]");
        }
        if required.contains(prop_name) {
            let _ = writeln!(out, "    /// Required by the specification.");
            let _ = writeln!(out, "    pub {field}: {ty},", ty = ty.render());
        } else {
            let _ = writeln!(out, "    /// Optional in the specification.");
            let _ = writeln!(
                out,
                "    #[serde(default, skip_serializing_if = \"Option::is_none\")]"
            );
            let _ = writeln!(out, "    pub {field}: Option<{ty}>,", ty = ty.render());
        }
    }

    let _ = writeln!(
        out,
        "    /// JSON members this type does not name.\n    ///\n    /// Preserved verbatim so a bridge can forward a field the peer sent that\n    /// this revision does not define. Empty rather than absent, and the two\n    /// serialise identically.\n    #[serde(flatten)]\n    pub extra: serde_json::Map<String, serde_json::Value>,"
    );
    let _ = writeln!(out, "}}\n");
}

/// Lower a schema node to a Rust type.
///
/// `anchor` is the name of the definition that owns `schema`. It exists only to
/// resolve a nested union to the enum emitted for that definition; a node that
/// carries its own union is otherwise indistinguishable from an inline one.
fn lower(schema: &Schema, defs: &BTreeMap<String, Schema>, anchor: &str) -> RustType {
    if let Some(target) = schema.def_ref() {
        if defs.contains_key(target) {
            return RustType::Named(type_ident(target));
        }
        // A reference the file does not define cannot become a named type, and
        // inventing one would not compile. A permissive field keeps the value
        // rather than dropping it, and the dangling check reports it.
        return RustType::Json;
    }

    // Order matters and must match `collect_unions`: `items` is checked before
    // `union` so a recursive batch type reuses its own name instead of minting
    // `..._m2` for a type that is the same one.
    if let Some(items) = &schema.items {
        return RustType::List(Box::new(lower(items, defs, anchor)));
    }

    if schema.union.is_some() {
        // The enum for a union is emitted under the name its path produces, the
        // same one `collect_unions` recorded it under.
        return RustType::Named(union_ident(anchor));
    }

    if let Some(value) = &schema.constant {
        return match value {
            Value::Bool(_) => RustType::Bool,
            Value::String(_) => RustType::Str,
            _ => RustType::Json,
        };
    }

    if schema.enum_values.is_some() {
        return RustType::Json;
    }

    // `allOf` on a property is an intersection; the merged shape is the
    // definition's own struct, and the member it most resembles is the best
    // available named type. Falling back to `Json` keeps a property that
    // intersects several things from being mistyped as just one of them.
    if !schema.intersect.is_empty() {
        return schema
            .intersect
            .iter()
            .find_map(|part| part.def_ref())
            .map_or(RustType::Json, |r| RustType::Named(type_ident(r)));
    }

    if schema.type_.is_none() {
        return RustType::Json;
    }

    match &schema.type_ {
        Some(TypeSet::Many(names)) => {
            // A multi-type selector is not a Rust type on its own, so each
            // distinct set gets a named alias emitted alongside the others.
            RustType::Named(multi_type_ident(names))
        }
        Some(TypeSet::One(name)) => scalar_named(name),
        None => RustType::Json,
    }
}

/// The alias name for a `"type": ["a", "b"]` selector.
fn multi_type_ident(names: &[String]) -> String {
    let mut out = String::from("MultiType");
    for name in names {
        out.extend(name.chars().map(|c| c.to_ascii_uppercase()));
    }
    out
}

fn scalar_named(name: &str) -> RustType {
    match name {
        "boolean" => RustType::Bool,
        "integer" => RustType::Int,
        "number" => RustType::Float,
        "string" => RustType::Str,
        // `object`, `array` and `null` all lower to `Value`: a free-form
        // embedded schema, a list whose element is unconstrained, and a null
        // that has no distinct Rust shape. The model already rejects any type
        // name it does not recognise, so there is nothing else to fall through
        // to here.
        _ => RustType::Json,
    }
}

/// Collect every `anyOf`/`oneOf` reachable from `schema`, naming each by the
/// path that reaches it.
///
/// The path is the name, not a counter: a counter would renumber every union
/// whenever an unrelated one is added upstream, which turns a schema change
/// into a diff across types that did not change. A path keeps the generated
/// name stable, which is what the drift check and any error message quoting a
/// type name both depend on.
fn collect_unions(
    schema: &Schema,
    owner: &str,
    path: &str,
    out: &mut Vec<(String, Union, String)>,
) {
    // The array check comes first, and deliberately. JSON-RPC batches are
    // recursive — `JSONRPCMessage` contains `Vec<JSONRPCMessage>` — so an
    // element union reached through an array has to reuse the array's own path
    // rather than extending it. Extending it would mint a new name for a type
    // that is the same one, and the two would drift apart on any schema change.
    if let Some(items) = &schema.items {
        collect_unions(items, owner, path, out);
    }
    if let Some(union) = &schema.union {
        out.push((union_ident(path), union.clone(), owner.to_owned()));
        for (i, member) in union.members.iter().enumerate() {
            collect_unions(member, owner, &format!("{path}_m{i}"), out);
        }
        return;
    }
    for (prop_name, prop) in &schema.properties {
        collect_unions(prop, owner, &format!("{path}_{prop_name}"), out);
    }
    for (i, part) in schema.intersect.iter().enumerate() {
        collect_unions(part, owner, &format!("{path}_all{i}"), out);
    }
}

/// Record every `"type": [..]` selector reachable from `schema`, so each
/// distinct set can be given a named alias.
fn collect_multi_types(schema: &Schema, out: &mut Vec<(String, String)>) {
    if let Some(TypeSet::Many(names)) = &schema.type_ {
        out.push((multi_type_ident(names), names.join(" or ")));
    }
    for prop in schema.properties.values() {
        collect_multi_types(prop, out);
    }
    if let Some(items) = &schema.items {
        collect_multi_types(items, out);
    }
    for part in &schema.intersect {
        collect_multi_types(part, out);
    }
    if let Some(union) = &schema.union {
        for member in &union.members {
            collect_multi_types(member, out);
        }
    }
}

/// A deterministic Rust identifier for the union at `path`.
///
/// The path is joined and then `PascalCased`, so a union nested two levels deep
/// is `CallToolResultContentChoice` rather than a snake-cased run-on. Naming
/// every segment in the path keeps two unions with the same member types but
/// different homes from colliding.
fn union_ident(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 6);
    for segment in path.split('_').filter(|s| !s.is_empty()) {
        let mut chars = segment.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    format!("{out}Choice")
}

/// The path to a union's `i`th member, as `collect_unions` would build it.
///
/// `ident` is an already-suffixed enum name, so the suffix is removed before
/// the member segment is appended; otherwise the name would pick up a second
/// `Choice` and disagree with the collector.
fn member_path(ident: &str, i: usize) -> String {
    let base = ident.strip_suffix("Choice").unwrap_or(ident);
    format!("{base}_m{i}")
}

/// Every `$ref` in `schema` that `defs` does not define.
///
/// `defs` is required, not optional: without it every reference looks dangling,
/// which makes the check useless rather than strict. A reference cycle is not
/// a problem here — the recursion follows definitions, and the depth is
/// bounded by the number of definitions.
fn collect_dangling(schema: &Schema, defs: &BTreeMap<String, Schema>) -> Option<Vec<String>> {
    if let Some(target) = schema.def_ref() {
        if !defs.contains_key(target) {
            return Some(vec![target.to_owned()]);
        }
        return None;
    }
    let mut out = Vec::new();
    for prop in schema.properties.values() {
        out.extend(collect_dangling(prop, defs).unwrap_or_default());
    }
    if let Some(items) = &schema.items {
        out.extend(collect_dangling(items, defs).unwrap_or_default());
    }
    if let Some(Union { members, .. }) = &schema.union {
        for member in members {
            out.extend(collect_dangling(member, defs).unwrap_or_default());
        }
    }
    for part in &schema.intersect {
        out.extend(collect_dangling(part, defs).unwrap_or_default());
    }
    (!out.is_empty()).then_some(out)
}

/// Render `text` as `///` doc lines, or `None` when there is nothing to say.
fn doc_lines(name: &str, description: Option<&str>, title: Option<&str>) -> Option<String> {
    // Upstream prose is copied verbatim, so a description that arrives as one
    // long paragraph is split at the first sentence boundary. The text is
    // unchanged; only the paragraph break is added, which is what
    // `doc_lazy_continuation` and `too_long_first_paragraph` ask for. Splitting
    // re-joining would change a user's documentation.
    let mut out = String::new();
    // A one-line summary naming the definition, so the type's own first doc
    // paragraph is a single line. The specification's prose then follows as a
    // second paragraph, which is what keeps `too_long_first_doc_paragraph` from
    // firing and reads better in rustdoc than a four-line lead.
    let _ = writeln!(out, "/// `{name}`.");
    if let Some(title) = title {
        let _ = writeln!(out, "///");
        let _ = writeln!(out, "/// {title}");
    }
    if let Some(description) = description {
        let _ = writeln!(out, "///");
        for line in description.lines() {
            let _ = writeln!(out, "/// {line}");
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The module name for a revision directory: `2025-11-25` → `v2025_11_25`.
///
/// Public because the writer that lays the generated files out needs the same
/// mapping, and two copies of it would be one more way for a filename and its
/// `mod` declaration to disagree.
pub fn module_ident(revision: &str) -> String {
    if revision == "draft" {
        return "draft".to_owned();
    }
    format!("v{}", revision.replace('-', "_"))
}

/// A definition name to a Rust type name.
///
/// The schema's own casing is kept, so `CallToolResult` becomes
/// `CallToolResult` rather than `Call_Tool_Result`. Converting to snake case
/// would lose the boundary the upstream author chose, and the whole point of
/// the shared names is that a codec layer can name them the same way in every
/// revision.
pub fn type_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            if i == 0 && ch.is_ascii_digit() {
                out.push('_');
            }
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if is_keyword(&out) {
        out.push('_');
    }
    out
}

/// A JSON member name to a Rust field name.
///
/// The original name is preserved in a serde rename so the wire format is
/// unaffected; a `field_ident` that differed from the member name would
/// otherwise be a silent wire change.
pub fn field_ident(name: &str) -> String {
    let snake = to_snake(name);
    if RESERVED.iter().any(|r| *r == snake) {
        format!("r#{snake}")
    } else if is_keyword(&snake) {
        format!("{snake}_")
    } else if snake.starts_with('_') {
        // JSON-RPC reserves members whose name begins with an underscore, and
        // upstream's schemas do carry one. `pub _meta` would trip
        // `used_underscore_binding` — the underscore prefix means "unused" in
        // Rust, not in JSON — so the field drops the leading underscore and
        // keeps a `#[serde(rename)]` back to the wire name. The rename is
        // already emitted by the caller, so the mapping stays exact.
        snake.trim_start_matches('_').to_owned()
    } else {
        snake
    }
}

fn to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

fn variant_ident(value: &str, index: usize) -> String {
    let upper: String = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if upper.is_empty() {
        format!("Member{index}")
    } else {
        upper
    }
}

/// Rust keywords that would collide with a raw identifier.
const RESERVED: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "if", "impl", "in",
    "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "try", "type",
    "typeof", "union", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

fn is_keyword(name: &str) -> bool {
    RESERVED.contains(&name)
}
