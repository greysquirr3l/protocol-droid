//! The primitives a registry holds: tools, resources, resource templates, and
//! prompts.
//!
//! # These are the specification's shapes, read from the vendored schemas
//!
//! Every field below is named by a `schema.json` in
//! `crates/protocol-droid-schema/spec/`, and the doc comment on each cites the
//! revision that introduced it. The rule this module follows is the project's
//! one: never remember a wire shape, read it. The two revisions that bracket
//! the range are `2024-11-05` and `2026-07-28`, and the field sets differ —
//! `2024-11-05`'s `Tool` is `{name, description, inputSchema}` and
//! `2026-07-28`'s adds `title`, `outputSchema`, `annotations`, `icons` and
//! `_meta` — so every field a later revision added is an `Option` here and the
//! oldest revision's three are the only non-optional ones.
//!
//! # Where a schema is stored, and where it is lowered
//!
//! [`Tool::input_schema`] is a [`JsonObject`], which holds the *original text*
//! rather than a re-serialised tree. That is what makes a per-revision schema
//! dialect swap a re-parse of known bytes rather than a lossy round trip, and
//! it is why the store keeps one copy per tool rather than one copy per
//! (tool, revision) pair: the dialect lowering is T08's codec's job, it is
//! deterministic given the bytes, and caching it per revision would mean a
//! cache to invalidate on every publish. The schema is validated once, here,
//! at the boundary — nothing downstream re-parses it just to check it is an
//! object.
//!
//! # What is *not* here
//!
//! No handler, no I/O, and no session. A tool that a server can call is
//! described by the [`Tool`] value; *calling* it is a handler the application
//! layer owns, and binding a name to that handler is the registry's problem
//! only in the sense that a missing name must be an error rather than a call
//! into nothing. The visibility rule in [`super::visibility`] is the only thing
//! in `domain/registry` that consults a session, and it does so through values
//! it is handed rather than by reaching for one.

use core::fmt;
use std::sync::Arc;

use crate::domain::Feature;
use crate::domain::ir::FeatureSet;
use crate::domain::ir::{Annotations, Extensions, JsonObject, ResourceIcon};
use crate::domain::registry::auth::{Scope, ScopeSet};

/// Which list a primitive belongs to.
///
/// The four lists are separate on the wire (`tools/list`, `resources/list`,
/// `resources/templates/list`, `prompts/list`) and they are notified on
/// separately, so the distinction is load-bearing rather than cosmetic: a tool
/// being added must not make a session's prompt list appear to have changed.
///
/// `Ord` and `PartialOrd` are derived from the same declaration order, and
/// `PrimitiveKind::ORDERING_AGREES` is the test that says so. The
/// declaration order is a display order chosen once — tools, then resources,
/// then templates, then prompts, matching the order the specification
/// introduces them — and it is not the order any peer observes, because
/// `Snapshot` keeps each list in registration order and this only orders the
/// _kinds_ when a change set is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrimitiveKind {
    /// `tools/list`, `tools/call`.
    Tool,
    /// `resources/list`, `resources/read`.
    Resource,
    /// `resources/templates/list`.
    ResourceTemplate,
    /// `prompts/list`, `prompts/get`.
    Prompt,
}

impl PrimitiveKind {
    /// Every kind, in declaration order.
    pub const ALL: [Self; 4] = [
        Self::Tool,
        Self::Resource,
        Self::ResourceTemplate,
        Self::Prompt,
    ];

    /// The name used in a log line and in a change set.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Resource => "resource",
            Self::ResourceTemplate => "resource_template",
            Self::Prompt => "prompt",
        }
    }

    /// The feature a session needs before it may list this kind.
    ///
    /// So the listing methods' gates are written once. A table here rather than
    /// at each call site is the point: `resources/list` and
    /// `resources/templates/list` are separate methods in the spec, and a
    /// second place naming which requires which feature is a second place to
    /// forget it.
    ///
    /// `ResourceTemplate` maps to [`Feature::Resources`], not to a feature of
    /// its own, because the specification's `resources` capability covers
    /// templates too. A peer that can list resources can list their templates;
    /// a separate feature would gate the two independently and let a session
    /// hold the odd combination the spec has no way to express.
    #[must_use]
    pub const fn feature(self) -> Feature {
        match self {
            Self::Tool => Feature::Tools,
            Self::Resource | Self::ResourceTemplate => Feature::Resources,
            Self::Prompt => Feature::Prompts,
        }
    }
}

impl fmt::Display for PrimitiveKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A primitive's name, or a resource's URI.
///
/// One newtype for all four kinds rather than a `ToolId`, a `ResourceId` and so
/// on, because they are looked up in one namespace-shaped map per kind and
/// nothing in the registry ever compares a tool's name against a prompt's. Four
/// newtypes would mean four identical `HashMap` key types and a conversion at
/// every boundary, and would buy nothing: a `ToolId` cannot be passed where a
/// prompt is expected, but neither can an `Arc<str>` that was obtained from a
/// different table.
///
/// `Arc<str>` and deliberately not `Copy`, for the same reason
/// [`RequestId`](crate::domain::RequestId) is: duplicating an identifier is
/// meaningless, and a clone that shares the same bytes is the point.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrimitiveId(Arc<str>);

impl PrimitiveId {
    /// The identifier's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The identifier's bytes, shared with every clone.
    #[must_use]
    pub fn into_arc(self) -> Arc<str> {
        self.0
    }
}

impl From<&str> for PrimitiveId {
    fn from(text: &str) -> Self {
        Self(Arc::from(text))
    }
}

impl From<String> for PrimitiveId {
    fn from(text: String) -> Self {
        Self(Arc::from(text))
    }
}

impl From<Arc<str>> for PrimitiveId {
    fn from(text: Arc<str>) -> Self {
        Self(text)
    }
}

impl fmt::Display for PrimitiveId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the registry must know about a primitive to decide who may see it.
///
/// Split from the primitive itself so the visibility predicate reads two small
/// values instead of a variant, and so a caller that already has the metadata
/// does not have to hold the whole tool to ask whether it is visible.
///
/// Deliberately not `Default`: a default would have to name one of the four
/// kinds, and a metadata value that silently claims to be a tool is worse than
/// one that does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrimitiveMeta {
    /// Which list it belongs to.
    kind: PrimitiveKind,
    /// What a session must be able to do for this primitive to be expressible.
    requires: FeatureSet,
    /// What a principal must hold for this primitive to be visible.
    scopes: ScopeSet,
}

impl PrimitiveMeta {
    /// Metadata for a primitive that needs nothing: every session with any
    /// principal sees it.
    #[must_use]
    pub const fn new(kind: PrimitiveKind) -> Self {
        Self {
            kind,
            requires: FeatureSet::empty(),
            scopes: ScopeSet::empty(),
        }
    }

    /// The features a session must have.
    #[must_use]
    pub const fn requires(mut self, features: FeatureSet) -> Self {
        self.requires = features;
        self
    }

    /// The scopes a principal must hold.
    #[must_use]
    pub const fn scopes(mut self, scopes: ScopeSet) -> Self {
        self.scopes = scopes;
        self
    }

    /// Which list it belongs to.
    #[must_use]
    pub const fn kind(self) -> PrimitiveKind {
        self.kind
    }

    /// What a session must be able to do.
    #[must_use]
    pub const fn required_features(self) -> FeatureSet {
        self.requires
    }

    /// What a principal must hold.
    #[must_use]
    pub const fn required_scopes(self) -> ScopeSet {
        self.scopes
    }
}

/// A tool a client can call.
///
/// `name` and `inputSchema` are required by every revision including
/// `2026-07-28`; everything else was added later and is `None` for a tool
/// authored against `2024-11-05`.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    /// `name`. Required by every revision.
    pub name: PrimitiveId,
    /// `title`. `2026-07-28`.
    pub title: Option<String>,
    /// `description`. Every revision.
    pub description: Option<String>,
    /// `inputSchema`. Required by every revision, and a JSON Schema object.
    ///
    /// Stored as the original text, validated once. See the module docs.
    pub input_schema: JsonObject,
    /// `outputSchema`. `2025-06-18`.
    pub output_schema: Option<JsonObject>,
    /// `annotations`. Typed in `2025-03-26`; untyped in nothing — the field
    /// itself first appears there.
    pub annotations: Option<ToolAnnotations>,
    /// `icons`. `2026-07-28`.
    pub icons: Option<Vec<ResourceIcon>>,
    /// `_meta` and any field a later revision adds.
    pub extensions: Extensions,
}

impl Tool {
    /// A tool with a name and its input schema, and nothing else.
    ///
    /// # Errors
    ///
    /// Returns the [`JsonObject`](crate::domain::ir::JsonObjectError)
    /// from parsing `input_schema`, which is the only fallible step: the schema
    /// is validated here, once, so nothing downstream has to.
    pub fn new(
        name: impl Into<PrimitiveId>,
        input_schema: &str,
    ) -> Result<Self, crate::domain::ir::JsonObjectError> {
        Ok(Self {
            name: name.into(),
            title: None,
            description: None,
            input_schema: JsonObject::new(input_schema)?,
            output_schema: None,
            annotations: None,
            icons: None,
            extensions: Extensions::empty(),
        })
    }

    /// A human-readable description.
    #[must_use]
    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// An output schema, which makes the feature `ToolStructuredOutput`
    /// (`2025-06-18`) a requirement of this tool.
    ///
    /// # Errors
    ///
    /// Propagates the parse failure for `schema`.
    pub fn with_output_schema(
        mut self,
        schema: &str,
    ) -> Result<Self, crate::domain::ir::JsonObjectError> {
        self.output_schema = Some(JsonObject::new(schema)?);
        Ok(self)
    }
}

/// A tool's hints.
///
/// Every field here is a **hint**, not a guarantee: the schema says so
/// directly, and says a client must never make a tool-use decision from an
/// untrusted server's hints. Modelled because a registry that drops them loses
/// data the specification defines.
///
/// Carrying hints does **not** raise a primitive's required features, and that
/// is deliberate — see [`Primitive::meta`]. A peer on an older revision ignores
/// hints it does not know and calls the tool all the same, so gating
/// visibility on them would hide a working tool for no gain.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolAnnotations {
    /// `title`.
    pub title: Option<String>,
    /// `readOnlyHint`. Absent means unknown, not false.
    pub read_only_hint: Option<bool>,
    /// `destructiveHint`.
    pub destructive_hint: Option<bool>,
    /// `idempotentHint`.
    pub idempotent_hint: Option<bool>,
    /// `openWorldHint`.
    pub open_world_hint: Option<bool>,
}

/// A resource the server can read.
///
/// `name` and `uri` are required by every revision. A resource is keyed in the
/// registry by its **uri**, not its name: `resources/read` takes a uri, and two
/// resources may share a name.
#[derive(Debug, Clone, PartialEq)]
pub struct Resource {
    /// `uri`. Required. Also the registry's key for this primitive.
    pub uri: PrimitiveId,
    /// `name`. Required.
    pub name: String,
    /// `title`. `2026-07-28`.
    pub title: Option<String>,
    /// `description`. Every revision.
    pub description: Option<String>,
    /// `mimeType`. Every revision.
    pub mime_type: Option<String>,
    /// `size`, in bytes, when known. `2026-07-28`.
    pub size: Option<u64>,
    /// `annotations`. Every revision, and the same shape as a content block's.
    pub annotations: Option<Annotations>,
    /// `icons`. `2026-07-28`.
    pub icons: Option<Vec<ResourceIcon>>,
    /// `_meta` and any field a later revision adds.
    pub extensions: Extensions,
}

impl Resource {
    /// A resource with a uri and a name, and nothing else.
    #[must_use]
    pub fn new(uri: impl Into<PrimitiveId>, name: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            name: name.into(),
            title: None,
            description: None,
            mime_type: None,
            size: None,
            annotations: None,
            icons: None,
            extensions: Extensions::empty(),
        }
    }
}

/// A pattern that produces resources.
///
/// Separate from [`Resource`] because the two are listed separately
/// (`resources/templates/list` against `resources/list`) and because a
/// template's `uriTemplate` is not a uri: matching it against a request is
/// template work, and storing one under the other's key would make a template
/// look like a resource that happens to exist.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceTemplate {
    /// `uriTemplate`. Required. The registry's key for this primitive.
    pub uri_template: PrimitiveId,
    /// `name`. Required.
    pub name: String,
    /// `title`. `2026-07-28`.
    pub title: Option<String>,
    /// `description`. Every revision.
    pub description: Option<String>,
    /// `mimeType`, for when every matching resource shares one.
    pub mime_type: Option<String>,
    /// `annotations`.
    pub annotations: Option<Annotations>,
    /// `icons`. `2026-07-28`.
    pub icons: Option<Vec<ResourceIcon>>,
    /// `_meta` and any field a later revision adds.
    pub extensions: Extensions,
}

impl ResourceTemplate {
    /// A template with a uri template and a name.
    #[must_use]
    pub fn new(uri_template: impl Into<PrimitiveId>, name: impl Into<String>) -> Self {
        Self {
            uri_template: uri_template.into(),
            name: name.into(),
            title: None,
            description: None,
            mime_type: None,
            annotations: None,
            icons: None,
            extensions: Extensions::empty(),
        }
    }
}

/// An argument a prompt accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptArgument {
    /// `name`.
    pub name: String,
    /// `description`.
    pub description: Option<String>,
    /// `title`. `2026-07-28`.
    pub title: Option<String>,
    /// `required`.
    pub required: bool,
}

/// A prompt or prompt template the server offers.
///
/// Only `name` is required, and that is true in every revision — a prompt with
/// no arguments and no description is a legal document.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    /// `name`. Required. The registry's key for this primitive.
    pub name: PrimitiveId,
    /// `title`. `2026-07-28`.
    pub title: Option<String>,
    /// `description`. Every revision.
    pub description: Option<String>,
    /// `arguments`. Every revision.
    pub arguments: Vec<PromptArgument>,
    /// `icons`. `2026-07-28`.
    pub icons: Option<Vec<ResourceIcon>>,
    /// `_meta` and any field a later revision adds.
    pub extensions: Extensions,
}

impl Prompt {
    /// A prompt with a name and no arguments.
    #[must_use]
    pub fn new(name: impl Into<PrimitiveId>) -> Self {
        Self {
            name: name.into(),
            title: None,
            description: None,
            arguments: Vec::new(),
            icons: None,
            extensions: Extensions::empty(),
        }
    }

    /// An argument this prompt accepts.
    #[must_use]
    pub fn with_argument(mut self, name: impl Into<String>, required: bool) -> Self {
        self.arguments.push(PromptArgument {
            name: name.into(),
            description: None,
            title: None,
            required,
        });
        self
    }
}

/// Any primitive, tagged with its kind.
///
/// The four payloads stay separate types rather than collapsing into one
/// struct with fourteen `Option` fields, because the fields that matter are not
/// shared: `Tool` has an input schema and no uri, `Resource` has a uri and no
/// input schema, and a single flat struct would let a resource be built with
/// an input schema and no way to say whether that was a mistake.
#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
    /// A tool.
    Tool(Tool),
    /// A resource.
    Resource(Resource),
    /// A resource template.
    ResourceTemplate(ResourceTemplate),
    /// A prompt.
    Prompt(Prompt),
}

impl Primitive {
    /// Which list this belongs to.
    #[must_use]
    pub const fn kind(&self) -> PrimitiveKind {
        match self {
            Self::Tool(_) => PrimitiveKind::Tool,
            Self::Resource(_) => PrimitiveKind::Resource,
            Self::ResourceTemplate(_) => PrimitiveKind::ResourceTemplate,
            Self::Prompt(_) => PrimitiveKind::Prompt,
        }
    }

    /// The key this primitive is filed under.
    #[must_use]
    pub const fn id(&self) -> &PrimitiveId {
        match self {
            Self::Tool(tool) => &tool.name,
            Self::Resource(resource) => &resource.uri,
            Self::ResourceTemplate(template) => &template.uri_template,
            Self::Prompt(prompt) => &prompt.name,
        }
    }

    /// The metadata a visibility decision is made from.
    ///
    /// Derived from the payload rather than stored beside it, so the two
    /// cannot disagree — a separate `meta` field would be a second source of
    /// truth for a thing the payload already determines.
    ///
    /// # What derives a requirement, and what does not
    ///
    /// A requirement is raised when a session **cannot correctly use the
    /// primitive** without the feature. That is a narrower test than "the
    /// primitive has a field the revision does not define", and the difference
    /// decides whether a feature is a gate or a filter.
    ///
    /// A tool's `outputSchema` is a gate. A tool that declares structured
    /// output answers with a `structuredContent` member, and a session that
    /// cannot represent it does not get a text fallback from the registry — it
    /// gets a result whose shape it will misread. This is the same
    /// `StructuredContentToText` decision the lowering path already made, and
    /// the registry has to agree with it or the two disagree about the same
    /// tool on the same session.
    ///
    /// Annotations, icons, `title` and `size` are **not** gates. An older peer
    /// ignores a hint it does not recognise and the tool still works, which is
    /// the same reason [`Annotations`] does not raise
    /// `required_features` on a content block. Requiring a feature for
    /// decoration would hide a perfectly callable tool from every session on an
    /// older revision, and the hiding is silent — the peer simply never sees
    /// the tool.
    #[must_use]
    pub fn meta(&self) -> PrimitiveMeta {
        let mut meta = PrimitiveMeta::new(self.kind());
        if let Self::Tool(tool) = self
            && tool.output_schema.is_some()
        {
            meta = meta.requires(FeatureSet::empty().with(Feature::ToolStructuredOutput));
        }
        meta
    }
}

/// A primitive as registered: the payload, plus the policy a visibility
/// decision is made from.
///
/// # Why the two are one value
///
/// [`PrimitiveMeta`] is *derived* from the payload — that is what stops a tool
/// carrying an `outputSchema` from being filed as needing nothing — but two of
/// the three inputs to a visibility decision are not derivable. A tool's
/// required scopes are a statement by whoever registered it, not a fact about
/// its schema, and a resource's required features depend on the revision it is
/// for.
///
/// So the snapshot stores a `Registered`, not a `Primitive`. One value means
/// the listing and the lookup cannot return a payload with one policy and a
/// neighbour with another, and a `PrimitiveMeta` is never handed to
/// [`is_visible`](super::is_visible) that was not stored beside the payload it
/// describes.
///
/// The derived half and the declared half are merged once, at construction.
/// From then on there is a single set of requirements per primitive, and
/// `requires` adds to the derived set rather than replacing it.
#[derive(Debug, Clone, PartialEq)]
pub struct Registered {
    primitive: Primitive,
    policy: PrimitiveMeta,
}

impl Registered {
    /// Registers `primitive` with no requirements beyond the derived ones.
    #[must_use]
    pub fn new(primitive: Primitive) -> Self {
        let policy = primitive.meta();
        Self { primitive, policy }
    }

    /// The payload.
    #[must_use]
    pub const fn primitive(&self) -> &Primitive {
        &self.primitive
    }

    /// The key this is filed under.
    #[must_use]
    pub const fn id(&self) -> &PrimitiveId {
        self.primitive.id()
    }

    /// Which list this belongs to.
    #[must_use]
    pub const fn kind(&self) -> PrimitiveKind {
        self.primitive.kind()
    }

    /// The requirements a visibility decision is made from.
    #[must_use]
    pub const fn meta(&self) -> &PrimitiveMeta {
        &self.policy
    }

    /// Adds to the required features rather than replacing them.
    #[must_use]
    pub const fn requiring(mut self, features: FeatureSet) -> Self {
        self.policy = self
            .policy
            .requires(self.policy.required_features().union(features));
        self
    }

    /// Adds to the required scopes rather than replacing them.
    #[must_use]
    pub const fn requiring_scope(mut self, scope: Scope) -> Self {
        self.policy = self
            .policy
            .scopes(self.policy.required_scopes().with(scope));
        self
    }
}

impl From<Primitive> for Registered {
    fn from(primitive: Primitive) -> Self {
        Self::new(primitive)
    }
}

impl From<Tool> for Primitive {
    fn from(tool: Tool) -> Self {
        Self::Tool(tool)
    }
}
impl From<Resource> for Primitive {
    fn from(resource: Resource) -> Self {
        Self::Resource(resource)
    }
}

impl From<ResourceTemplate> for Primitive {
    fn from(template: ResourceTemplate) -> Self {
        Self::ResourceTemplate(template)
    }
}

impl From<Prompt> for Primitive {
    fn from(prompt: Prompt) -> Self {
        Self::Prompt(prompt)
    }
}

#[cfg(test)]
mod tests {
    use core::cmp::Ordering;
    use std::sync::Arc;

    use super::{
        Primitive, PrimitiveId, PrimitiveKind, PrimitiveMeta, Prompt, Resource, ResourceTemplate,
        Tool,
    };
    use crate::domain::Feature;
    use crate::domain::ir::FeatureSet;
    use crate::domain::ir::JsonObjectError;
    use crate::domain::registry::auth::{Scope, ScopeSet};

    /// The identifier a primitive is filed under is the field its own list
    /// method looks it up by: a resource by uri, everything else by name. Two
    /// resources sharing a name is legal, so keying resources by name would
    /// silently drop one of them.
    #[test]
    fn the_key_is_the_field_the_list_method_uses() -> Result<(), JsonObjectError> {
        let cases = [
            (
                Primitive::from(Tool::new("t", "{}")?),
                PrimitiveKind::Tool,
                "t",
            ),
            (
                Primitive::from(Resource::new("file:///a", "a")),
                PrimitiveKind::Resource,
                "file:///a",
            ),
            (
                Primitive::from(ResourceTemplate::new("file:///{id}", "by-id")),
                PrimitiveKind::ResourceTemplate,
                "file:///{id}",
            ),
            (
                Primitive::from(Prompt::new("greet")),
                PrimitiveKind::Prompt,
                "greet",
            ),
        ];
        for (primitive, kind, key) in cases {
            assert_eq!(primitive.kind(), kind);
            assert_eq!(primitive.id().as_str(), key);
        }
        Ok(())
    }

    /// `Ord` and `PartialOrd` derived from one declaration, which the 1.98
    /// derive fast path assumes. A test inside a single module cannot catch a
    /// disagreement between the two derives, because both come from the same
    /// source; what it can catch is a later edit that derives one and hand-writes
    /// the other.
    #[test]
    fn ordering_agrees_in_both_directions() {
        for a in PrimitiveKind::ALL {
            for b in PrimitiveKind::ALL {
                let partial = a.partial_cmp(&b);
                assert_eq!(partial, Some(a.cmp(&b)), "{a} against {b}");
                assert_eq!(a < b, partial == Some(Ordering::Less), "{a} against {b}");
                assert_eq!(a > b, partial == Some(Ordering::Greater), "{a} against {b}");
            }
        }
    }

    /// The declaration order is the order the specification introduces the
    /// lists, and it is the order a change set renders its kinds in. Pinning it
    /// means a reordering that changes a notification's rendered form shows up
    /// as a test failure rather than as a diff in a log.
    #[test]
    fn the_kind_order_is_the_specification_order() {
        assert_eq!(
            PrimitiveKind::ALL,
            [
                PrimitiveKind::Tool,
                PrimitiveKind::Resource,
                PrimitiveKind::ResourceTemplate,
                PrimitiveKind::Prompt,
            ]
        );
    }

    /// A tool's metadata is derived from its payload. A tool carrying an output
    /// schema needs `ToolStructuredOutput`, and one that does not must not
    /// claim to — the gate reads this, so getting it wrong hides or exposes a
    /// tool on a session that cannot express it.
    #[test]
    fn metadata_follows_the_payload() -> Result<(), JsonObjectError> {
        let plain = Tool::new("t", "{}")?;
        assert!(Primitive::from(plain).meta().required_features().is_empty());

        let structured = Tool::new("t", "{}")?.with_output_schema(r#"{"type":"object"}"#)?;
        let required = Primitive::from(structured).meta().required_features();
        assert!(required.contains(Feature::ToolStructuredOutput));
        Ok(())
    }

    #[test]
    fn scopes_are_carried_by_the_metadata_not_the_payload() {
        let meta = PrimitiveMeta::new(PrimitiveKind::Tool)
            .scopes(ScopeSet::empty().with(Scope::Write))
            .requires(FeatureSet::empty().with(Feature::Tools));
        assert!(meta.required_scopes().contains(Scope::Write));
        assert!(meta.required_features().contains(Feature::Tools));
        assert_eq!(meta.kind(), PrimitiveKind::Tool);
    }

    /// Metadata that states no rule requires nothing, so the primitive is
    /// visible to everyone — the one "allow" case, expressed as an empty
    /// subset rather than as a branch.
    #[test]
    fn metadata_without_a_rule_requires_nothing() {
        let meta = PrimitiveMeta::new(PrimitiveKind::Resource);
        assert!(meta.required_features().is_empty());
        assert!(meta.required_scopes().holds(ScopeSet::empty()));
    }

    /// An `Arc<str>` identifier shares its bytes across clones, which is why
    /// the type is not `Copy`.
    #[test]
    fn identifiers_share_their_bytes() {
        let id = PrimitiveId::from("tools/call");
        let clone = id.clone();
        assert_eq!(id, clone);
        assert!(Arc::ptr_eq(&id.clone().into_arc(), &clone.into_arc()));
        assert_eq!(id.to_string(), "tools/call");
    }

    /// The tool constructors are the only fallible thing here, and they fail on
    /// exactly the input a schema check should reject.
    #[test]
    fn a_non_object_schema_is_refused_at_the_boundary() {
        assert!(Tool::new("t", "[]").is_err());
        assert!(Tool::new("t", "{oops").is_err());
        assert!(
            Tool::new("t", "{}")
                .and_then(|t| t.with_output_schema("[]"))
                .is_err()
        );
    }
}
