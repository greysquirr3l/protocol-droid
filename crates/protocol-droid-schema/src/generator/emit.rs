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

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::Value;

use super::schema_model::{JsonKind, Schema, SchemaFile, TypeSet, Union};

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

    // Each union's dispatch is decided before anything is emitted: a union
    // that cannot be dispatched on a pinned member needs the dispatcher helper,
    // and the helper has to be in the module before the union that uses it.
    let plans: Vec<UnionPlan> = pending
        .iter()
        .map(|(ident, union, doc_name)| {
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
            UnionPlan {
                ident: ident.clone(),
                doc_name: doc_name.clone(),
                members,
                discriminator: dispatch_discriminator(&union.members, &file.defs),
                shapes: ShapeTable::build(&union.members, &file.defs),
            }
        })
        .collect();

    if plans.iter().any(|plan| plan.discriminator.is_none()) {
        emit_dispatcher(&mut source);
    }

    for plan in &plans {
        emit_union(&mut source, plan);
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

/// The property a union's members are told apart by, and the value each
/// member pins it to.
///
/// The specification discriminates its unions in more than one way. Content
/// blocks carry `{"type": "image", "const": ...}`; requests and notifications
/// carry `{"method": "tools/call", "const": ...}`. Both are the same shape of
/// fact — a member pins one property to one value — so the generator looks for
/// the fact rather than for the property's name.
struct Discriminator {
    /// The property's name.
    property: String,
    /// The value each member pins it to, in member order.
    values: Vec<String>,
}

/// The properties the specification discriminates with, most specific first.
///
/// `type` comes first because it is the one the specification's own prose uses
/// for content blocks, and the one the generated code's error messages name.
/// `method` is next because it is how every request and notification is
/// discriminated, and a union that discriminates on `method` is the common
/// case rather than the exception.
const DISCRIMINATORS: &[&str] = &["type", "method", "mode", "resultType"];

/// The constants a union member's own properties fix.
///
/// A member that is not a reference, or whose target declares no pinned
/// property, yields `None`; the caller then knows the member fixes nothing
/// and cannot take part in a tag dispatch.
fn member_constants<'a>(
    member: &'a Schema,
    defs: &'a BTreeMap<String, Schema>,
) -> Option<BTreeMap<&'a str, &'a str>> {
    let target = member.def_ref()?;
    let definition = defs.get(target)?;
    Some(
        definition
            .merged_properties(defs)
            .into_iter()
            .filter_map(|(name, property)| {
                let value = property.constant.as_ref()?.as_str()?;
                Some((name, value))
            })
            .collect(),
    )
}

/// The property a union's members are told apart by, if one is.
///
/// `None` means the union cannot be dispatched on a single pinned property,
/// which happens in two ways: a member pins nothing, or two of them pin the
/// same property to the same value. The second case is not a gap in the
/// specification but a fact about it — a union whose members nest their own
/// unions can have three members all tagged `string`, and no `match` on that
/// can tell them apart however it is written.
///
/// Dispatch also needs the discriminant to be *injective*: a tag that
/// identifies exactly one alternative. And it needs the tag to be *required*,
/// not merely pinned.
///
/// `ElicitRequestParams` is the case that makes the last rule matter. Its
/// two alternatives pin `mode` to `form` and to `url`, and both pin it in
/// their own definition — but only the URL one *requires* it. A client that
/// sends a form elicitation without the tag sends a value the specification
/// accepts, and a dispatcher that insists on reading the tag rejects it.
///
/// A tag the specification requires is a different matter: the peer has
/// promised to send it, so reading it cannot fail on a conforming value.
fn dispatch_discriminator(
    members: &[Box<Schema>],
    defs: &BTreeMap<String, Schema>,
) -> Option<Discriminator> {
    let constants: Vec<BTreeMap<&str, &str>> = members
        .iter()
        .map(|member| member_constants(member, defs))
        .collect::<Option<Vec<_>>>()?;

    // The candidates are the properties every member pins. A property only one
    // member pins cannot discriminate: the others would answer to any value
    // of it.
    let mut candidates: Vec<&str> = constants
        .first()
        .map(|first| first.keys().copied().collect())
        .unwrap_or_default();
    candidates.retain(|name| constants.iter().all(|member| member.contains_key(name)));

    // The specification's own discriminators come first, then whatever else the
    // members pin, in the order the first member declares them.
    candidates.sort_by_key(|name| {
        DISCRIMINATORS
            .iter()
            .position(|preferred| preferred == name)
            .unwrap_or(usize::MAX)
    });

    // A candidate every member *requires* is a tag the peer has promised to
    // send. One that is merely pinned is not: the schema allows the member
    // without it.
    let required: Vec<BTreeSet<String>> = members
        .iter()
        .map(|member| member.merged_required(defs).into_iter().collect())
        .collect();

    for property in candidates {
        let values: Vec<&str> = constants
            .iter()
            .filter_map(|member| member.get(property).copied())
            .collect();
        if values.len() != members.len() {
            continue;
        }
        if !required.iter().all(|names| names.contains(property)) {
            continue;
        }
        let unique = values.iter().collect::<BTreeSet<_>>();
        if unique.len() == values.len() {
            return Some(Discriminator {
                property: property.to_owned(),
                values: values.into_iter().map(str::to_owned).collect(),
            });
        }
    }
    None
}

/// The shape of a value, as the dispatcher sees it.
///
/// A shape is what a value *can* be: the kind it has, the members it must
/// carry, and — where the value stands for a union — the alternatives it may
/// take. The dispatcher uses the shape to reject an arm whose member the
/// value does not fit, which is what tells two structurally similar
/// alternatives apart.
#[derive(Debug, Clone)]
struct Shape {
    /// The kind the shape accepts.
    kind: JsonKind,
    /// The members an object of this shape must carry.
    required: Vec<String>,
    /// The shapes this shape accepts, when it stands for a union of them.
    alternatives: Vec<usize>,
}

/// One arm of a union, as the dispatcher sees it.
///
/// The arms of a union are told apart by the properties they name, the
/// members they require, and the shape of the members that carry the
/// distinction — not by the order the specification lists them in. A value
/// that satisfies more than one arm belongs to the arm that accounts for more
/// of the value's members.
struct Arm {
    /// The JSON kind this arm accepts.
    ///
    /// The kind is part of the arm's identity: two alternatives can lower to
    /// the same Rust type and still accept different JSON. `JSONObject` and
    /// `MultiType` both lower to `serde_json::Value`, and only the kind tells
    /// them apart.
    kind: JsonKind,
    /// The members this arm requires.
    required: Vec<String>,
    /// Every member this arm names.
    named: Vec<String>,
    /// The shape of the members whose shape tells this arm apart, and the
    /// index of that shape in the union's table.
    ///
    /// Only members whose shape carries information are listed. A member
    /// holding a scalar is the same in every arm that has it, so listing it
    /// would cost bytes and decide nothing.
    members: Vec<(String, usize)>,
    /// The shape of this arm's elements, and the index of that shape in the
    /// union's table, when the arm is an array.
    elements: Option<usize>,
    /// The properties this arm pins, and the values it allows for them.
    ///
    /// A property the specification pins to one value, or to a closed set of
    /// values, is a discriminator: a value carrying a different one is not
    /// this arm's.
    constraints: Vec<(String, Vec<String>)>,
}

/// Everything one union needs in order to be emitted.
struct UnionPlan {
    /// The name of the generated enum.
    ident: String,
    /// The name the specification gives the union, for the doc comment.
    doc_name: String,
    /// The Rust type of each alternative.
    members: Vec<RustType>,
    /// The property the alternatives are told apart by, when one exists.
    discriminator: Option<Discriminator>,
    /// The shapes the alternatives' members have.
    shapes: ShapeTable,
}

/// The shapes of a union's members, and the arms that use them.
struct ShapeTable {
    /// Every shape, indexed by the number the arms refer to.
    shapes: Vec<Shape>,
    /// The arms, in the specification's order.
    arms: Vec<Arm>,
}

/// How deep a schema is followed while a shape is built.
///
/// A shape never descends into an object's own properties — only into a
/// union's alternatives — so the vendored schemas do not reach this bound.
/// It is here so that a future recursive union cannot hang the generator.
const MAX_SHAPE_DEPTH: usize = 8;

impl ShapeTable {
    /// Builds the shapes and arms for one union.
    fn build(members: &[Box<Schema>], defs: &BTreeMap<String, Schema>) -> Self {
        let mut table = Self {
            shapes: Vec::new(),
            arms: Vec::new(),
        };
        for member in members {
            let arm = table.arm_of(member, defs);
            table.arms.push(arm);
        }
        table.drop_lonely_constraints();
        table
    }

    /// Builds the arm for one alternative of the union.
    fn arm_of(&mut self, member: &Schema, defs: &BTreeMap<String, Schema>) -> Arm {
        let properties = member.merged_properties(defs);
        let mut named: Vec<String> = properties.keys().map(|name| (*name).to_owned()).collect();
        named.sort();

        let mut members = Vec::new();
        for (name, property) in &properties {
            let index = self.shape_of(property, defs, 0);
            let Some(shape) = self.shapes.get(index) else {
                continue;
            };
            if shape.informative() {
                members.push(((*name).to_owned(), index));
            }
        }

        let elements = self.elements_of(member, defs);

        let mut constraints = Vec::new();
        for (name, property) in &properties {
            let allowed = pinned_values(property);
            if let Some(allowed) = allowed {
                constraints.push(((*name).to_owned(), allowed));
            }
        }

        Arm {
            kind: member.json_kind(defs),
            required: member.merged_required(defs),
            named,
            members,
            elements,
            constraints,
        }
    }

    /// The shape of an array member's elements.
    ///
    /// A batch is told apart from a single message by what its elements are,
    /// so the element shape is the only thing that separates the two.
    fn elements_of(&mut self, member: &Schema, defs: &BTreeMap<String, Schema>) -> Option<usize> {
        if member.json_kind(defs) != JsonKind::Array {
            return None;
        }
        let items = member.items.as_deref()?;
        let index = self.shape_of(items, defs, 0);
        self.shapes.get(index).map(|_| index)
    }

    /// Builds the shape of a schema node, and the index it lands at.
    fn shape_of(&mut self, node: &Schema, defs: &BTreeMap<String, Schema>, depth: usize) -> usize {
        if depth > MAX_SHAPE_DEPTH {
            return self.push(JsonKind::Any, Vec::new(), Vec::new());
        }
        if let Some(union) = &node.union {
            let alternatives = union
                .members
                .iter()
                .map(|member| self.shape_of(member, defs, depth + 1))
                .collect();
            return self.push(JsonKind::Any, Vec::new(), alternatives);
        }
        if let Some(target) = node.def_ref() {
            return match defs.get(target) {
                Some(definition) => self.shape_of(definition, defs, depth + 1),
                None => self.push(JsonKind::Any, Vec::new(), Vec::new()),
            };
        }
        match node.json_kind(defs) {
            JsonKind::Object => self.push(JsonKind::Object, node.merged_required(defs), Vec::new()),
            JsonKind::Array => self.push(JsonKind::Array, Vec::new(), Vec::new()),
            JsonKind::Scalar => self.push(JsonKind::Scalar, Vec::new(), Vec::new()),
            JsonKind::Any => self.push(JsonKind::Any, Vec::new(), Vec::new()),
        }
    }

    /// Records a shape and returns the index it lands at.
    fn push(&mut self, kind: JsonKind, required: Vec<String>, alternatives: Vec<usize>) -> usize {
        self.shapes.push(Shape {
            kind,
            required,
            alternatives,
        });
        self.shapes.len() - 1
    }

    /// Keeps only the constraints that more than one arm shares.
    ///
    /// A constraint applies only to a property that *several* arms constrain.
    /// A property one arm alone constrains — a `format` only the string
    /// schema names, say — is that arm's own business, and enforcing it here
    /// would reject values the specification's own type accepts.
    fn drop_lonely_constraints(&mut self) {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for arm in &self.arms {
            for (name, _) in &arm.constraints {
                *counts.entry(name.clone()).or_default() += 1;
            }
        }
        let shared: BTreeSet<String> = counts
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|(name, _)| name)
            .collect();
        for arm in &mut self.arms {
            arm.constraints.retain(|(name, _)| shared.contains(name));
        }
    }
}

impl Shape {
    /// True when this shape carries information worth putting in the table.
    ///
    /// A scalar shape is the same in every arm that has it, and a shape that
    /// accepts anything carries nothing at all. Neither is worth a slot.
    const fn informative(&self) -> bool {
        matches!(self.kind, JsonKind::Object | JsonKind::Array) || !self.alternatives.is_empty()
    }
}

/// Emits the doc comment and derive line of a union.
fn emit_union_header(source: &mut String, doc_name: &str, discriminator: Option<&Discriminator>) {
    match discriminator {
        Some(discriminator) => {
            let _ = writeln!(
                source,
                "/// One of the alternatives the specification allows for `{doc_name}`.\n\
                 ///\n\
                 /// Dispatched on the `{property}` member. Every alternative fixes\n\
                 /// its own `{property}`, so the JSON *does* say which one was sent\n\
                 /// even though it is not wrapped \u{2014} and matching on member order\n\
                 /// instead would be guessing. Two alternatives that differ only in\n\
                 /// a member the JSON does not carry are the same shape on the wire,\n\
                 /// so tried in order the second is unreachable and the value is\n\
                 /// re-emitted as the first, silently and without an error.\n\
                 ///\n\
                 /// An unrecognised or absent `{property}` is a decode failure rather\n\
                 /// than a fall-through, so a malformed message cannot be quietly\n\
                 /// accepted as whichever alternative happens to parse.",
                property = discriminator.property
            );
        }
        None => {
            let _ = writeln!(
                source,
                "/// One of the alternatives the specification allows for `{doc_name}`.\n\
                 ///\n\
                 /// Dispatched on shape: the alternatives are not told apart by a\n\
                 /// single pinned member, so the value is matched against each\n\
                 /// alternative's own kind, required members and the shape of the\n\
                 /// members that carry the distinction. The alternative that\n\
                 /// accounts for more of the value wins, and the specification's\n\
                 /// own order breaks a tie.\n\
                 ///\n\
                 /// Matching on member order alone would be guessing, and the\n\
                 /// guess is silently wrong: the alternatives overlap, so a value\n\
                 /// meant for one of them lands in whichever comes first and is\n\
                 /// re-emitted with that alternative's shape."
            );
        }
    }
    // `Serialize` keeps `untagged`; only `Deserialize` is hand-written. The
    // attribute has to stay even though the manual impl ignores it, or the
    // enum encodes as `{"Member0": {...}}` rather than as the member.
    let _ = writeln!(source, "#[derive(Debug, Clone, PartialEq, Serialize)]");
}

/// Emits one union, dispatched on a pinned member when the tags allow it and
/// on shape when they do not.
fn emit_union(source: &mut String, plan: &UnionPlan) {
    let UnionPlan {
        ident,
        doc_name,
        members,
        discriminator,
        shapes,
    } = plan;
    let ident = ident.as_str();
    let doc_name = doc_name.as_str();

    emit_union_header(source, doc_name, discriminator.as_ref());

    let _ = writeln!(source, "#[serde(untagged)]");
    let _ = writeln!(source, "pub enum {ident} {{");
    for (i, member) in members.iter().enumerate() {
        let _ = writeln!(source, "    /// Alternative {i}.", i = i + 1);
        let _ = writeln!(source, "    Member{i}({ty}),", ty = member.render());
    }
    let _ = writeln!(source, "}}\n");

    // The tables live at module scope rather than inside the deserializer:
    // a union with a dozen alternatives would otherwise push the function
    // past the length a reader — and the linter — will tolerate.
    if discriminator.is_none() {
        emit_shape_tables(source, ident, shapes);
    }

    let _ = writeln!(source, "impl<'de> serde::Deserialize<'de> for {ident} {{");
    // `Result` is fully qualified because the specification defines a type of
    // that name, and it is in scope in every generated module: an unqualified
    // `Result<Self, D::Error>` resolves to the MCP one, which takes no
    // generic arguments.
    let _ = writeln!(
        source,
        "    fn deserialize<D>(deserializer: D) -> ::core::result::Result<Self, D::Error>\n    \
         where\n        D: serde::Deserializer<'de>,\n    {{"
    );

    match discriminator {
        Some(discriminator) => {
            emit_tag_dispatch(source, ident, doc_name, discriminator);
        }
        None => emit_shape_dispatch(source, ident, doc_name, members),
    }

    let _ = writeln!(source, "    }}");
    let _ = writeln!(source, "}}\n");
}

/// The name a generated union's tables take, in the upper-case style Rust
/// uses for constants and statics.
///
/// `ServerResultChoice` becomes `SERVER_RESULT_CHOICE`, so the tables read as
/// the module-level constants they are.
fn screaming_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 8);
    let mut previous_was_lower = false;
    for character in name.chars() {
        if character.is_uppercase() && previous_was_lower {
            out.push('_');
        }
        previous_was_lower = character.is_lowercase() || character.is_ascii_digit();
        out.extend(character.to_uppercase());
    }
    out
}

/// Emits the name of a JSON kind, as the generated dispatcher spells it.
const fn emit_kind(kind: JsonKind) -> &'static str {
    match kind {
        JsonKind::Object => "Object",
        JsonKind::Array => "Array",
        JsonKind::Scalar => "Scalar",
        JsonKind::Any => "Any",
    }
}

/// The values a property is pinned to, if the specification fixes any.
///
/// A property pinned to a constant allows exactly that value; a property
/// restricted to a closed set of constants allows those. A property that
/// pins nothing allows everything, and pins nothing that a union's arms could
/// be told apart by.
fn pinned_values(property: &Schema) -> Option<Vec<String>> {
    if let Some(value) = &property.constant {
        return value.as_str().map(|text| vec![text.to_owned()]);
    }
    property.enum_values.as_ref().and_then(|values| {
        values
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
    })
}

/// Emits a list of names as a Rust slice literal.
fn emit_str_slice(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Emits the deserializer of a union that is dispatched on a pinned member.
///
/// The member is one the specification *requires* of every alternative, so
/// the peer has promised to send it and reading it cannot fail on a
/// conforming value.
fn emit_tag_dispatch(
    source: &mut String,
    ident: &str,
    doc_name: &str,
    discriminator: &Discriminator,
) {
    let _ = writeln!(
        source,
        "        let value = serde_json::Value::deserialize(deserializer)?;\n\
         \x20       let tag = value\n            .get(\"{property}\")\n            .and_then(serde_json::Value::as_str)\n            .ok_or_else(|| serde::de::Error::custom(\"a `{doc_name}` must carry a `{property}` string\"))?;\n\
         \x20       match tag {{",
        property = discriminator.property
    );
    for (i, tag) in discriminator.values.iter().enumerate() {
        let _ = writeln!(
            source,
            "            \"{tag}\" => serde_json::from_value(value)\n                \
             .map({ident}::Member{i})\n                \
             .map_err(<D::Error as serde::de::Error>::custom),"
        );
    }
    let _ = writeln!(
        source,
        "            other => Err(<D::Error as serde::de::Error>::custom(format!(\n                \
         \"`{{other}}` is not a {doc_name}; expected one of {listed}\"))),",
        listed = discriminator.values.join(", ")
    );
    let _ = writeln!(source, "        }}");
}

/// Emits the tables a shape-dispatched union dispatches on.
///
/// The tables live at module scope rather than inside the deserializer: a
/// union with a dozen alternatives would otherwise push the function past
/// the length a reader — and the linter — will tolerate.
fn emit_shape_tables(source: &mut String, ident: &str, shapes: &ShapeTable) {
    let _ = writeln!(
        source,
        "static {table}_SHAPES: &[UnionShape] = &[",
        table = screaming_snake(ident)
    );
    for shape in &shapes.shapes {
        let _ = writeln!(
            source,
            "    UnionShape {{ kind: UnionKind::{kind}, required: &[{required}], alternatives: &[{alternatives}] }},",
            kind = emit_kind(shape.kind),
            required = emit_str_slice(&shape.required),
            alternatives = shape
                .alternatives
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let _ = writeln!(source, "];\n");
    let _ = writeln!(
        source,
        "static {table}_ARMS: &[UnionArm] = &[",
        table = screaming_snake(ident)
    );
    for arm in &shapes.arms {
        let _ = writeln!(
            source,
            "    UnionArm {{\n        \
             kind: UnionKind::{kind},\n        \
             required: &[{required}],\n        \
             named: &[{named}],\n        \
             members: &[{members}],\n        \
             elements: {elements},\n        \
             constraints: &[{constraints}],\n    }},",
            kind = emit_kind(arm.kind),
            required = emit_str_slice(&arm.required),
            named = emit_str_slice(&arm.named),
            members = arm
                .members
                .iter()
                .map(|(name, index)| format!("(\"{name}\", {index})"))
                .collect::<Vec<_>>()
                .join(", "),
            elements = arm
                .elements
                .map_or_else(|| "None".to_owned(), |index| format!("Some({index})")),
            constraints = arm
                .constraints
                .iter()
                .map(|(name, allowed)| {
                    let values = allowed
                        .iter()
                        .map(|value| format!("\"{value}\""))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("(\"{name}\", &[{values}])")
                })
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let _ = writeln!(source, "];\n");
}

/// Emits the deserializer of a union that is dispatched on shape.
///
/// The alternatives are not told apart by a single pinned member, so the
/// value is matched against each alternative's own kind, required members
/// and the shape of the members that carry the distinction. The
/// alternative that accounts for more of the value wins, and the
/// specification's own order breaks a tie.
fn emit_shape_dispatch(source: &mut String, ident: &str, doc_name: &str, members: &[RustType]) {
    let table = screaming_snake(ident);
    let _ = writeln!(
        source,
        "        let value = serde_json::Value::deserialize(deserializer)?;\n\
         \x20       let mut order: Vec<usize> = (0..{table}_ARMS.len())\n            \
         .filter(|index| {{\n            \
         {table}_ARMS\n            \
         .get(*index)\n            \
         .is_some_and(|arm| arm.accepts({table}_SHAPES, &value))\n        \
         }})\n            \
         .collect();\n\
         \x20       order.sort_by_key(|index| {{\n            \
         let arm = {table}_ARMS.get(*index);\n            \
         ::core::cmp::Reverse((\n            \
         arm.map_or(0, |arm| arm.coverage(&value)),\n            \
         arm.map_or(0, |arm| arm.required.len()),\n            \
         ))\n        }});\n\
         \x20       let mut failure = String::new();\n\
         \x20       for index in order {{\n            \
         let member = match index {{"
    );
    for (i, member) in members.iter().enumerate() {
        let _ = writeln!(
            source,
            "                {i} => <{ty} as serde::Deserialize>::deserialize(&value)\n                    \
             .map({ident}::Member{i}),",
            ty = member.render()
        );
    }
    let _ = writeln!(
        source,
        "                _ => continue,\n            }};\n            \
         match member {{\n                \
         Ok(member) => return Ok(member),\n                \
         Err(error) => failure = error.to_string(),\n            }}\n        }}\n\
         \x20       Err(<D::Error as serde::de::Error>::custom(format!(\n            \
         \"a `{doc_name}` matches none of its alternatives: {{failure}}\")))"
    );
}

/// Emits the helper a shape-dispatched union needs.
///
/// The dispatcher picks the arm a value belongs to; it does not validate the
/// value. A property the specification pins is enforced here only when more
/// than one arm pins it, because a constraint one arm alone carries is that
/// arm's own business — and the generated type does not enforce it either, so
/// enforcing it in the dispatcher would make the two disagree about the same
/// value.
fn emit_dispatcher(source: &mut String) {
    emit_union_kind(source);
    emit_union_shape(source);
    emit_union_arm(source);
    emit_union_impls(source);
}

/// Emits the `UnionKind` enum the generated dispatcher uses.
fn emit_union_kind(source: &mut String) {
    let _ = writeln!(
        source,
        "/// The JSON kind a value has.\n\
         ///\n\
         /// The kind is part of an arm's identity. Two alternatives can lower to\n\
         /// the same Rust type and still accept different JSON, and only the kind\n\
         /// tells them apart: a `JSONObject` and a `MultiType` are both\n\
         /// `serde_json::Value` on the Rust side and an object and a scalar on\n\
         /// the wire.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
         enum UnionKind {{\n\
         \x20   /// A JSON object.\n\
         \x20   Object,\n\
         \x20   /// A JSON array.\n\
         \x20   Array,\n\
         \x20   /// A JSON scalar: a string, a number, a boolean or null.\n\
         \x20   Scalar,\n\
         \x20   /// Any JSON value.\n\
         \x20   Any,\n\
         }}\n"
    );
}

/// Emits the `UnionShape` struct the generated dispatcher uses.
fn emit_union_shape(source: &mut String) {
    let _ = writeln!(
        source,
        "/// The shape of a value, as the dispatcher sees it.\n\
         ///\n\
         /// A shape is what a value *can* be: the kind it has, the members it\n\
         /// must carry, and — where the value stands for a union — the\n\
         /// alternatives it may take.\n\
         struct UnionShape {{\n\
         \x20   /// The kind the shape accepts.\n\
         \x20   kind: UnionKind,\n\
         \x20   /// The members an object of this shape must carry.\n\
         \x20   required: &'static [&'static str],\n\
         \x20   /// The shapes this shape accepts, when it stands for a union of them.\n\
         \x20   alternatives: &'static [usize],\n\
         }}\n\
         \n\
         impl UnionShape {{\n\
         \x20   /// True when the value could have this shape.\n\
         \x20   fn accepts(&self, shapes: &[UnionShape], value: &serde_json::Value) -> bool {{\n\
         \x20       if !self.alternatives.is_empty() {{\n\
         \x20           return self.alternatives.iter().any(|index| {{\n\
         \x20               shapes\n\
         \x20                   .get(*index)\n\
         \x20                   .is_some_and(|shape| shape.accepts(shapes, value))\n\
         \x20           }});\n\
         \x20       }}\n\
         \x20       if self.kind != UnionKind::Any && self.kind != union_kind(value) {{\n\
         \x20           return false;\n\
         \x20       }}\n\
         \x20       if self.kind == UnionKind::Object {{\n\
         \x20           let Some(object) = value.as_object() else {{\n\
         \x20               return false;\n\
         \x20           }};\n\
         \x20           return self.required.iter().all(|name| object.contains_key(*name));\n\
         \x20       }}\n\
         \x20       true\n\
         \x20   }}\n\
         }}\n"
    );
}

/// Emits the `UnionArm` struct the generated dispatcher uses.
fn emit_union_arm(source: &mut String) {
    let _ = writeln!(
        source,
        "/// One arm of a union, as the dispatcher sees it.\n\
         ///\n\
         /// The arms of a union are told apart by the properties they name, the\n\
         /// members they require, and the shape of the members that carry the\n\
         /// distinction — not by the order the specification lists them in. A\n\
         /// value that satisfies more than one arm belongs to the arm that\n\
         /// accounts for more of the value's members.\n\
         struct UnionArm {{\n\
         \x20   /// The kind this arm accepts.\n\
         \x20   kind: UnionKind,\n\
         \x20   /// The members this arm requires.\n\
         \x20   required: &'static [&'static str],\n\
         \x20   /// Every member this arm names.\n\
         \x20   named: &'static [&'static str],\n\
         \x20   /// The shape of the members whose shape tells this arm apart, and\n\
         \x20   /// the index of that shape in the union's table.\n\
         \x20   members: &'static [(&'static str, usize)],\n\
         \x20   /// The shape of this arm's elements, and the index of that shape\n\
         \x20   /// in the union's table, when the arm is an array.\n\
         \x20   elements: Option<usize>,\n\
         \x20   /// The properties this arm pins, and the values it allows for them.\n\
         \x20   ///\n\
         \x20   /// A property the specification pins to one value, or to a closed set of\n\
         \x20   /// values, is a discriminator: a value carrying a different one is not\n\
         \x20   /// this arm's.\n\
         \x20   constraints: &'static [(&'static str, &'static [&'static str])],\n\
         }}\n"
    );
}

/// Emits the behaviour of the generated dispatcher's helpers.
fn emit_union_impls(source: &mut String) {
    let _ = writeln!(
        source,
        "impl UnionArm {{\n\
         \x20   /// True when the value could be this arm's.\n\
         \x20   ///\n\
         \x20   /// This is a dispatch test, not a validation: it decides which arm a\n\
         \x20   /// value belongs to, and the value is then decoded into that arm.\n\
         \x20   fn accepts(&self, shapes: &[UnionShape], value: &serde_json::Value) -> bool {{\n\
         \x20       if self.kind != union_kind(value) {{\n\
         \x20           return false;\n\
         \x20       }}\n\
         \x20       match self.kind {{\n\
         \x20           UnionKind::Object => {{\n\
         \x20               let Some(object) = value.as_object() else {{\n\
         \x20                   return false;\n\
         \x20               }};\n\
         \x20               if !self.required.iter().all(|name| object.contains_key(*name)) {{\n\
         \x20                   return false;\n\
         \x20               }}\n\
         \x20               for (name, allowed) in self.constraints {{\n\
         \x20                   let Some(member) = object.get(*name) else {{\n\
         \x20                       continue;\n\
         \x20                   }};\n\
         \x20                   if !member.as_str().is_some_and(|text| allowed.contains(&text)) {{\n\
         \x20                       return false;\n\
         \x20                   }}\n\
         \x20               }}\n\
         \x20               for (name, index) in self.members {{\n\
         \x20                   let Some(member) = object.get(*name) else {{\n\
         \x20                       continue;\n\
         \x20                   }};\n\
         \x20                   let Some(shape) = shapes.get(*index) else {{\n\
         \x20                       continue;\n\
         \x20                   }};\n\
         \x20                   if !shape.accepts(shapes, member) {{\n\
         \x20                       return false;\n\
         \x20                   }}\n\
         \x20               }}\n\
         \x20               true\n\
         \x20           }}\n\
         \x20           UnionKind::Array => {{\n\
         \x20               let Some(elements) = value.as_array() else {{\n\
         \x20                   return false;\n\
         \x20               }};\n\
         \x20               let Some(index) = self.elements else {{\n\
         \x20                   return true;\n\
         \x20               }};\n\
         \x20               let Some(shape) = shapes.get(index) else {{\n\
         \x20                   return true;\n\
         \x20               }};\n\
         \x20               elements.iter().all(|element| shape.accepts(shapes, element))\n\
         \x20           }}\n\
         \x20           _ => true,\n\
         \x20       }}\n\
         \x20   }}\n\
         \n\
         \x20   /// How many of the value's members this arm accounts for.\n\
         \x20   fn coverage(&self, value: &serde_json::Value) -> usize {{\n\
         \x20       let Some(object) = value.as_object() else {{\n\
         \x20           return 0;\n\
         \x20       }};\n\
         \x20       object.keys().filter(|name| self.named.contains(&name.as_str())).count()\n\
         \x20   }}\n\
         }}\n\
         \n\
         /// The JSON kind a value has.\n\
         const fn union_kind(value: &serde_json::Value) -> UnionKind {{\n\
         \x20   match value {{\n\
         \x20       serde_json::Value::Object(_) => UnionKind::Object,\n\
         \x20       serde_json::Value::Array(_) => UnionKind::Array,\n\
         \x20       _ => UnionKind::Scalar,\n\
         \x20   }}\n\
         }}\n"
    );
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
///
/// A member name that is a Rust keyword becomes a raw identifier (`type` →
/// `r#type`), except for the four keywords rustc refuses to spell that way —
/// `self`, `Self`, `super` and `crate` — which take a trailing underscore
/// instead, because `r#self` is a compile error rather than an identifier.
/// `Self` is covered by the `self` entry: the name is lowercased first.
///
/// A name that starts with a digit gains a leading underscore (`2fa` →
/// `_2fa`), since a Rust identifier cannot begin with one.
pub fn field_ident(name: &str) -> String {
    let snake = to_snake(name);
    if NOT_RAW_IDENTIFIABLE.contains(&snake.as_str()) {
        format!("{snake}_")
    } else if RESERVED.iter().any(|r| *r == snake) {
        format!("r#{snake}")
    } else if snake.starts_with(|c: char| c.is_ascii_digit()) {
        // Checked before the underscore trim below, which would strip the very
        // underscore that makes the name legal. The serde rename the caller
        // emits keeps the wire name exact.
        format!("_{snake}")
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
///
/// `gen` is here because Rust 2024 reserves it, and this crate is edition
/// 2024 — a member named `gen` would emit a bare `gen` field, which does not
/// compile. `union` is not reserved in 2024, but it is a plausible MCP member
/// name and reserving it costs nothing.
const RESERVED: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "try", "type",
    "typeof", "union", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// The reserved words that rustc will not accept as a raw identifier.
///
/// `r#self`, `r#Self`, `r#super` and `r#crate` are all rejected — the raw
/// identifier syntax is not available for these four. `Self` is not listed
/// because it never gets here: `to_snake` lowercases it to `self`. Every other word in
/// [`RESERVED`] may be spelled `r#word`.
///
/// The list is matched against the lowercased snake form of a member name, so
/// a member named `Self` arrives here as `self` and is covered by that entry.
const NOT_RAW_IDENTIFIABLE: &[&str] = &["self", "super", "crate"];

fn is_keyword(name: &str) -> bool {
    RESERVED.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::{NOT_RAW_IDENTIFIABLE, RESERVED, field_ident};

    #[test]
    fn words_rustc_rejects_as_raw_identifiers_take_a_trailing_underscore() {
        for (name, want) in [
            ("self", "self_"),
            ("Self", "self_"),
            ("super", "super_"),
            ("crate", "crate_"),
        ] {
            assert_eq!(field_ident(name), want, "member {name:?}");
        }
    }

    #[test]
    fn other_reserved_words_become_raw_identifiers() {
        for (name, want) in [
            ("type", "r#type"),
            ("enum", "r#enum"),
            ("gen", "r#gen"),
            ("union", "r#union"),
        ] {
            assert_eq!(field_ident(name), want, "member {name:?}");
        }
    }

    #[test]
    fn leading_digits_and_underscores_are_made_legal() {
        for (name, want) in [
            ("2fa", "_2fa"),
            ("_meta", "meta"),
            ("$schema", "schema"),
            ("inputSchema", "input_schema"),
        ] {
            assert_eq!(field_ident(name), want, "member {name:?}");
        }
    }

    #[test]
    fn no_reserved_word_is_ever_emitted_bare_or_raw_when_rustc_forbids_it() {
        for word in RESERVED {
            let ident = field_ident(word);
            assert_ne!(ident, *word, "{word:?} must not be emitted bare");
            if NOT_RAW_IDENTIFIABLE.contains(&word.to_ascii_lowercase().as_str()) {
                assert!(!ident.starts_with("r#"), "{word:?} became {ident:?}");
            }
        }
    }
}
