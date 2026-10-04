//! `Feature` — the protocol behaviours a session may or may not have, and
//! [`resolve`] — the pure function that decides which of them it has.
//!
//! # The table is the model
//!
//! [`FEATURES`] is a const table, and [`resolve`] is a fold over it. That is
//! the whole design: there is no second place where a feature's availability is
//! written down, so a row cannot disagree with the code that consults it, and
//! adding a revision is a data change rather than a code change.
//!
//! Each row answers four questions, all of them from the vendored changelogs:
//!
//! - **Who must support it** — [`Side`]. A server feature with no client to
//!   call it is off.
//! - **What capability it needs** — at most one client and one server flag.
//!   This is the prerequisite rule, and it is a requirement rather than a hint:
//!   a peer that did not advertise `elicitation` cannot be sent an elicitation
//!   request, whatever the revision allows.
//! - **Which transport it needs** — `None` for transport-independent features.
//!   Legacy SSE exists only on `2024-11-05`, so a feature that is only
//!   expressible there cannot be on for a `2025-03-26` peer.
//! - **Which revisions define it** — [`first`](FeatureSpec::first) and
//!   [`last`](FeatureSpec::last). A feature that was later *removed* has a
//!   `last`, and the removal is as much a part of the matrix as the addition.
//!
//! # Every row cites its changelog
//!
//! The doc comment on each variant names the revision and changelog entry that
//! introduced or removed it. A row without a citation is a failing task, and
//! [`tests::every_row_is_cited_and_ordered`] enforces the ordering half of that
//! mechanically: a removal must be attributed to a revision strictly after the
//! one that added it.
//!
//! # Removals are features of the matrix
//!
//! `2026-07-28` removes `ping`, `logging/setLevel` and
//! `notifications/roots/list_changed`, and moves `tasks` out of the core
//! protocol into an extension. Modelling those as rows with a `last` — rather
//! than as their absence — is what lets a bridge report *why* it cannot lower a
//! message, which is the difference between a diagnosable failure and a silent
//! drop.

use crate::domain::ir::FeatureSet;
use crate::domain::{
    Capability, CapabilitySet, ProtocolVersion, Side, TransportKind, transport_allowed,
};

/// `str` equality by hand, because `==` on slices is not what this needs and
/// a name makes the two call sites in `bit` and `bit_of` read the same.
fn str_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut i = 0;
    while i < left.len() {
        match (left.get(i), right.get(i)) {
            (Some(l), Some(r)) if l == r => {}
            _ => return false,
        }
        i += 1;
    }
    true
}

/// A protocol behaviour a session may have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    // ── Server: tools ──────────────────────────────────────────────────────
    /// `tools/list` and `tools/call`. Every revision, every transport.
    Tools,
    /// `tools.list_changed`, gated on the server advertising it.
    ToolsListChanged,
    /// Tool annotations — read-only, destructive, and so on. `2025-03-26`.
    ToolAnnotations,
    /// `structuredContent` on a tool result. `2025-06-18`.
    ToolStructuredOutput,
    /// Resource links in a tool call result. `2025-06-18`.
    ToolResourceLinks,
    /// `annotations.lastModified` on a content block. `2025-06-18`.
    ///
    /// Separate from `ToolAnnotations` because the two gate different
    /// revisions: annotations as a field exist in `2024-11-05`, untyped, while
    /// `lastModified` first appears in the typed form at `2025-06-18`. A value
    /// carrying only `audience` or `priority` needs the older feature alone.
    ToolAnnotationsLastModified,
    /// `icons` on a resource link. `2025-11-25`, SEP-973.
    ///
    /// `https://github.com/modelcontextprotocol/modelcontextprotocol/issues/973`
    ToolResourceLinkIcons,
    /// Audio content blocks. `2025-03-26`.
    ToolAudioContent,
    /// `resultType` on tool results. `2026-07-28`, required rather than
    /// optional there.
    ToolResultType,

    // ── Server: resources and prompts ──────────────────────────────────────
    /// `resources/list`, `resources/read`, `resources/templates/list`. Every
    /// revision.
    Resources,
    /// `resources.list_changed`.
    ResourcesListChanged,
    /// Server-to-client resource change notifications. The capability survives
    /// `2026-07-28` even though the methods behind it are replaced.
    ResourcesSubscribe,
    /// `prompts/list` and `prompts/get`. Every revision.
    Prompts,
    /// `prompts.list_changed`.
    PromptsListChanged,

    // ── Server: completions ────────────────────────────────────────────────
    /// `completion/complete`. `2025-03-26`.
    Completions,

    // ── Client features the server may use ─────────────────────────────────
    /// `roots/list`. Deprecated `2026-07-28` but still defined.
    Roots,
    /// `notifications/roots/list_changed`. **Removed** in `2026-07-28`.
    RootsListChanged,
    /// `sampling/createMessage`. Deprecated `2026-07-28` but still defined.
    Sampling,
    /// Sampling with `includeContext`. `2025-11-25`.
    SamplingContext,
    /// Sampling with `tools` and `toolChoice`. `2025-11-25`, SEP-1577.
    SamplingTools,
    /// `elicitation/create`. `2025-06-18`.
    Elicitation,
    /// Form-mode elicitation with the SEP-1330 schema shapes. `2025-11-25`.
    ElicitationForm,
    /// URL-mode elicitation. `2025-11-25`, SEP-1036. The completion
    /// notification behind it is removed in `2026-07-28`; the capability is
    /// not.
    ElicitationUrl,
    /// `logging/setLevel` and `notifications/message`. Deprecated `2026-07-28`,
    /// which also removes the method and moves log level into `_meta`.
    Logging,

    // ── Protocol level ─────────────────────────────────────────────────────
    /// JSON-RPC batching. Added `2025-03-26`, **removed** `2025-06-18`.
    JsonRpcBatching,
    /// The Streamable HTTP transport. `2025-03-26`.
    StreamableHttp,
    /// The legacy HTTP+SSE transport. `2024-11-05` only; replaced in
    /// `2025-03-26`, reclassified as Deprecated in `2026-07-28`.
    LegacySse,
    /// The `MCP-Protocol-Version` header on subsequent HTTP requests.
    /// `2025-06-18`.
    McpProtocolVersionHeader,
    /// `_meta` on interface types. `2025-06-18`.
    MetaExtensions,
    /// The stateless lifecycle: no `initialize` handshake, version and
    /// capabilities in `_meta` on every request. `2026-07-28`, SEP-2575.
    StatelessLifecycle,
    /// `server/discover`. `2026-07-28`, SEP-2575.
    ServerDiscover,
    /// `subscriptions/listen`, replacing the HTTP GET endpoint and
    /// `resources/subscribe`. `2026-07-28`, SEP-2575.
    SubscriptionsListen,
    /// `Mcp-Method` and `Mcp-Name` routing headers. `2026-07-28`, SEP-2243.
    StandardRoutingHeaders,
    /// `ttlMs` and `cacheScope` on cacheable results. `2026-07-28`, SEP-2549.
    ResponseCaching,
    /// The multi-round-trip request pattern, which is what the removal of
    /// `notifications/elicitation/complete` is predicated on. `2026-07-28`.
    MultiRoundTripRequests,
    /// `ping`. **Removed** in `2026-07-28`, SEP-2575.
    Ping,

    // ── Extensions ─────────────────────────────────────────────────────────
    /// The `tasks` capability, core-protocol only. Added `2025-11-25` (SEP-1686),
    /// **removed** from the core in `2026-07-28` when it moves to the
    /// `io.modelcontextprotocol/tasks` extension (SEP-2663).
    Tasks,
    /// The `extensions` capability bag. `2026-07-28`, SEP-2575.
    Extensions,
    /// The `experimental` capability bag. Every revision.
    Experimental,
}

impl Feature {
    /// Every feature, in declaration order.
    pub const ALL: [Self; 39] = [
        Self::Tools,
        Self::ToolsListChanged,
        Self::ToolAnnotations,
        Self::ToolStructuredOutput,
        Self::ToolResourceLinks,
        Self::ToolAnnotationsLastModified,
        Self::ToolResourceLinkIcons,
        Self::ToolAudioContent,
        Self::ToolResultType,
        Self::Resources,
        Self::ResourcesListChanged,
        Self::ResourcesSubscribe,
        Self::Prompts,
        Self::PromptsListChanged,
        Self::Completions,
        Self::Roots,
        Self::RootsListChanged,
        Self::Sampling,
        Self::SamplingContext,
        Self::SamplingTools,
        Self::Elicitation,
        Self::ElicitationForm,
        Self::ElicitationUrl,
        Self::Logging,
        Self::JsonRpcBatching,
        Self::StreamableHttp,
        Self::LegacySse,
        Self::McpProtocolVersionHeader,
        Self::MetaExtensions,
        Self::StatelessLifecycle,
        Self::ServerDiscover,
        Self::SubscriptionsListen,
        Self::StandardRoutingHeaders,
        Self::ResponseCaching,
        Self::MultiRoundTripRequests,
        Self::Ping,
        Self::Tasks,
        Self::Extensions,
        Self::Experimental,
    ];

    /// The feature's name, for logs, errors and golden files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::ToolsListChanged => "tools.listChanged",
            Self::ToolAnnotations => "tool.annotations",
            Self::ToolStructuredOutput => "tool.structuredOutput",
            Self::ToolResourceLinks => "tool.resourceLinks",
            Self::ToolAnnotationsLastModified => "tool.annotations.lastModified",
            Self::ToolResourceLinkIcons => "tool.resourceLinks.icons",
            Self::ToolAudioContent => "tool.audioContent",
            Self::ToolResultType => "tool.resultType",
            Self::Resources => "resources",
            Self::ResourcesListChanged => "resources.listChanged",
            Self::ResourcesSubscribe => "resources.subscribe",
            Self::Prompts => "prompts",
            Self::PromptsListChanged => "prompts.listChanged",
            Self::Completions => "completions",
            Self::Roots => "roots",
            Self::RootsListChanged => "roots.listChanged",
            Self::Sampling => "sampling",
            Self::SamplingContext => "sampling.context",
            Self::SamplingTools => "sampling.tools",
            Self::Elicitation => "elicitation",
            Self::ElicitationForm => "elicitation.form",
            Self::ElicitationUrl => "elicitation.url",
            Self::Logging => "logging",
            Self::JsonRpcBatching => "jsonRpcBatching",
            Self::StreamableHttp => "streamableHttp",
            Self::LegacySse => "legacySse",
            Self::McpProtocolVersionHeader => "mcpProtocolVersionHeader",
            Self::MetaExtensions => "metaExtensions",
            Self::StatelessLifecycle => "statelessLifecycle",
            Self::ServerDiscover => "server.discover",
            Self::SubscriptionsListen => "subscriptions.listen",
            Self::StandardRoutingHeaders => "standardRoutingHeaders",
            Self::ResponseCaching => "responseCaching",
            Self::MultiRoundTripRequests => "multiRoundTripRequests",
            Self::Ping => "ping",
            Self::Tasks => "tasks",
            Self::Extensions => "extensions",
            Self::Experimental => "experimental",
        }
    }
}

impl core::fmt::Display for Feature {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One row of the feature matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureSpec {
    /// The feature this row describes.
    pub feature: Feature,
    /// Which side has to support it.
    pub side: Side,
    /// Client capability required, if any.
    pub client_cap: Option<Capability>,
    /// Server capability required, if any.
    pub server_cap: Option<Capability>,
    /// Transport required, if any.
    pub transport: Option<TransportKind>,
    /// The oldest revision that defines it.
    pub first: ProtocolVersion,
    /// The revision that removed it, if it was removed.
    pub last: Option<ProtocolVersion>,
}

impl FeatureSpec {
    /// Whether `version` defines this feature, ignoring capabilities and
    /// transport.
    ///
    /// An unknown revision defines nothing, matching
    /// [`transport_allowed`](crate::domain::transport_allowed): there are no
    /// wire types to lower to.
    #[must_use]
    pub fn defined_in(&self, version: ProtocolVersion) -> bool {
        if !version.is_known() || version < self.first {
            return false;
        }
        self.last.is_none_or(|last| version < last)
    }
}

/// Every capability and revision constant, aliased so the table below reads as
/// a matrix rather than as a wall of paths.
///
/// # Why `tools`, `resources` and `prompts` are gated
///
/// These are capabilities in the vendored schemas, not merely collections the
/// server always has: a `ServerCapabilities` with no `tools` key is a server
/// with no tools. So a feature row for them carries `server_cap`, and a peer
/// that never sent `tools` never gets [`Feature::Tools`] — which is what stops
/// the resolver handing a client a `tools/list` it should not make.
///
/// The five tool sub-behaviours all require [`CapTools`] and nothing more,
/// because the schemas have no separate flag for "this server annotates its
/// tools". A sub-behaviour is therefore only on when the parent is.
use Capability::{
    Completions as CapCompletions, Elicitation as CapElicitation,
    ElicitationForm as CapElicitationForm, ElicitationUrl as CapElicitationUrl,
    Experimental as CapExperimental, Extensions as CapExtensions, Logging as CapLogging,
    Prompts as CapPrompts, PromptsListChanged as CapPromptsListChanged, Resources as CapResources,
    ResourcesListChanged as CapResourcesListChanged, ResourcesSubscribe as CapResourcesSubscribe,
    Roots as CapRoots, RootsListChanged as CapRootsListChanged, Sampling as CapSampling,
    SamplingContext as CapSamplingContext, SamplingTools as CapSamplingTools, Tasks as CapTasks,
    Tools as CapTools, ToolsListChanged as CapToolsListChanged,
};
use ProtocolVersion::{V2024_11_05, V2025_03_26, V2025_06_18, V2025_11_25, V2026_07_28};

/// The feature matrix.
///
/// Every row is cited in the doc comment on its [`Feature`] variant. The
/// ordering within a row is the same as the four questions in the module docs,
/// so the table can be read as a column of answers.
pub const FEATURES: &[FeatureSpec] = &[
    // ── Server: tools ──────────────────────────────────────────────────────
    FeatureSpec {
        feature: Feature::Tools,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapTools),
        transport: None,
        first: V2024_11_05,
        last: None,
    },
    FeatureSpec {
        feature: Feature::ToolsListChanged,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapToolsListChanged),
        transport: None,
        first: V2024_11_05,
        last: None,
    },
    // PR #185, `2025-03-26`: "Added comprehensive tool annotations".
    FeatureSpec {
        feature: Feature::ToolAnnotations,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapTools),
        transport: None,
        first: V2025_03_26,
        last: None,
    },
    // `2025-06-18`: `lastModified` is the first annotation field that the
    // typed `Annotations` object carries and `2025-03-26` does not. The
    // annotation field itself is older than that and untyped at `2024-11-05`,
    // so it is `ToolAnnotations` that gates the field rather than this.
    FeatureSpec {
        feature: Feature::ToolAnnotationsLastModified,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapTools),
        transport: None,
        first: V2025_06_18,
        last: None,
    },
    // SEP-973, `2025-11-25` major change 2: "Allow servers to expose icons as
    // additional metadata for tools, resources, resource templates, and
    // prompts". `https://github.com/modelcontextprotocol/modelcontextprotocol/issues/973`
    FeatureSpec {
        feature: Feature::ToolResourceLinkIcons,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapTools),
        transport: None,
        first: V2025_11_25,
        last: None,
    },
    // PR #371, `2025-06-18` major change 2.
    FeatureSpec {
        feature: Feature::ToolStructuredOutput,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapTools),
        transport: None,
        first: V2025_06_18,
        last: None,
    },
    // PR #603, `2025-06-18` major change 7.
    FeatureSpec {
        feature: Feature::ToolResourceLinks,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapTools),
        transport: None,
        first: V2025_06_18,
        last: None,
    },
    // `2025-03-26`: "Added support for audio data".
    FeatureSpec {
        feature: Feature::ToolAudioContent,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapTools),
        transport: None,
        first: V2025_03_26,
        last: None,
    },
    // `2026-07-28` major change 10 loosens the schemas; `resultType` is
    // required there.
    FeatureSpec {
        feature: Feature::ToolResultType,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapTools),
        transport: None,
        first: V2026_07_28,
        last: None,
    },
    // ── Server: resources and prompts ──────────────────────────────────────
    FeatureSpec {
        feature: Feature::Resources,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapResources),
        transport: None,
        first: V2024_11_05,
        last: None,
    },
    FeatureSpec {
        feature: Feature::ResourcesListChanged,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapResourcesListChanged),
        transport: None,
        first: V2024_11_05,
        last: None,
    },
    // The capability outlives the methods: `2026-07-28` replaces
    // `resources/subscribe` with `subscriptions/listen` but keeps the flag.
    FeatureSpec {
        feature: Feature::ResourcesSubscribe,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapResourcesSubscribe),
        transport: None,
        first: V2024_11_05,
        last: None,
    },
    FeatureSpec {
        feature: Feature::Prompts,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapPrompts),
        transport: None,
        first: V2024_11_05,
        last: None,
    },
    FeatureSpec {
        feature: Feature::PromptsListChanged,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapPromptsListChanged),
        transport: None,
        first: V2024_11_05,
        last: None,
    },
    // `2025-03-26`: "Added `completions` capability". The capability is the
    // only way a server signals support, so the row requires it.
    FeatureSpec {
        feature: Feature::Completions,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapCompletions),
        transport: None,
        first: V2025_03_26,
        last: None,
    },
    // ── Client features the server may use ─────────────────────────────────
    // Roots: deprecated by SEP-2577 in `2026-07-28`, still defined.
    FeatureSpec {
        feature: Feature::Roots,
        side: Side::Client,
        client_cap: Some(CapRoots),
        server_cap: None,
        transport: None,
        first: V2024_11_05,
        last: None,
    },
    // Removed with `notifications/roots/list_changed`, SEP-2575.
    FeatureSpec {
        feature: Feature::RootsListChanged,
        side: Side::Client,
        client_cap: Some(CapRootsListChanged),
        server_cap: None,
        transport: None,
        first: V2024_11_05,
        last: Some(V2026_07_28),
    },
    // Sampling: deprecated by SEP-2577 in `2026-07-28`, still defined.
    FeatureSpec {
        feature: Feature::Sampling,
        side: Side::Client,
        client_cap: Some(CapSampling),
        server_cap: None,
        transport: None,
        first: V2024_11_05,
        last: None,
    },
    FeatureSpec {
        feature: Feature::SamplingContext,
        side: Side::Client,
        client_cap: Some(CapSamplingContext),
        server_cap: None,
        transport: None,
        first: V2025_11_25,
        last: None,
    },
    // SEP-1577, `2025-11-25`.
    FeatureSpec {
        feature: Feature::SamplingTools,
        side: Side::Client,
        client_cap: Some(CapSamplingTools),
        server_cap: None,
        transport: None,
        first: V2025_11_25,
        last: None,
    },
    // PR #382, `2025-06-18` major change 6.
    FeatureSpec {
        feature: Feature::Elicitation,
        side: Side::Client,
        client_cap: Some(CapElicitation),
        server_cap: None,
        transport: None,
        first: V2025_06_18,
        last: None,
    },
    // SEP-1330, `2025-11-25` minor change 5.
    FeatureSpec {
        feature: Feature::ElicitationForm,
        side: Side::Client,
        client_cap: Some(CapElicitationForm),
        server_cap: None,
        transport: None,
        first: V2025_11_25,
        last: None,
    },
    // SEP-1036, `2025-11-25` major change 6.
    FeatureSpec {
        feature: Feature::ElicitationUrl,
        side: Side::Client,
        client_cap: Some(CapElicitationUrl),
        server_cap: None,
        transport: None,
        first: V2025_11_25,
        last: None,
    },
    // SEP-2577, `2026-07-28`, deprecates it and removes `logging/setLevel`.
    FeatureSpec {
        feature: Feature::Logging,
        side: Side::Server,
        client_cap: None,
        server_cap: Some(CapLogging),
        transport: None,
        first: V2024_11_05,
        last: None,
    },
    // ── Protocol level ─────────────────────────────────────────────────────
    // PR #228 adds it in `2025-03-26`; PR #416 removes it in `2025-06-18`.
    FeatureSpec {
        feature: Feature::JsonRpcBatching,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: None,
        first: V2025_03_26,
        last: Some(V2025_06_18),
    },
    // PR #206, `2025-03-26`.
    FeatureSpec {
        feature: Feature::StreamableHttp,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: Some(TransportKind::StreamableHttp),
        first: V2025_03_26,
        last: None,
    },
    // `2024-11-05` only. Reclassified Deprecated by SEP-2596 in `2026-07-28`,
    // but that is a recommendation, not a removal — it was already gone in
    // `2025-03-26` as far as the versioned matrix is concerned.
    FeatureSpec {
        feature: Feature::LegacySse,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: Some(TransportKind::LegacySse),
        first: V2024_11_05,
        last: Some(V2025_03_26),
    },
    // PR #548, `2025-06-18` major change 8.
    FeatureSpec {
        feature: Feature::McpProtocolVersionHeader,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: None,
        first: V2025_06_18,
        last: None,
    },
    // PR #710, `2025-06-18`: `_meta` on additional interface types.
    FeatureSpec {
        feature: Feature::MetaExtensions,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: None,
        first: V2025_06_18,
        last: None,
    },
    // SEP-2575, `2026-07-28` major change 2.
    FeatureSpec {
        feature: Feature::StatelessLifecycle,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: None,
        first: V2026_07_28,
        last: None,
    },
    // SEP-2575, `2026-07-28` major change 3.
    FeatureSpec {
        feature: Feature::ServerDiscover,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: None,
        first: V2026_07_28,
        last: None,
    },
    // SEP-2575, `2026-07-28` major change 4.
    FeatureSpec {
        feature: Feature::SubscriptionsListen,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: None,
        first: V2026_07_28,
        last: None,
    },
    // SEP-2243, `2026-07-28` major change 4.
    FeatureSpec {
        feature: Feature::StandardRoutingHeaders,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: None,
        first: V2026_07_28,
        last: None,
    },
    // SEP-2549, `2026-07-28` major change 5.
    FeatureSpec {
        feature: Feature::ResponseCaching,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: None,
        first: V2026_07_28,
        last: None,
    },
    // `2026-07-28` major change 11, the pattern that replaces the elicitation
    // completion notification.
    FeatureSpec {
        feature: Feature::MultiRoundTripRequests,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: None,
        first: V2026_07_28,
        last: None,
    },
    // SEP-2575, `2026-07-28` major change 5: `ping` is removed outright.
    FeatureSpec {
        feature: Feature::Ping,
        side: Side::Server,
        client_cap: None,
        server_cap: None,
        transport: None,
        first: V2024_11_05,
        last: Some(V2026_07_28),
    },
    // ── Extensions ─────────────────────────────────────────────────────────
    // SEP-1686 adds it in `2025-11-25`; SEP-2663 moves it to
    // `io.modelcontextprotocol/tasks` in `2026-07-28`.
    FeatureSpec {
        feature: Feature::Tasks,
        side: Side::Both,
        client_cap: Some(CapTasks),
        server_cap: Some(CapTasks),
        transport: None,
        first: V2025_11_25,
        last: Some(V2026_07_28),
    },
    // SEP-2575, `2026-07-28`.
    FeatureSpec {
        feature: Feature::Extensions,
        side: Side::Both,
        client_cap: Some(CapExtensions),
        server_cap: Some(CapExtensions),
        transport: None,
        first: V2026_07_28,
        last: None,
    },
    FeatureSpec {
        feature: Feature::Experimental,
        side: Side::Both,
        client_cap: Some(CapExperimental),
        server_cap: Some(CapExperimental),
        transport: None,
        first: V2024_11_05,
        last: None,
    },
];

/// The set of features a negotiated session has.
///
/// A `u128` bitset, so [`allows`](Self::allows) is a mask and a test — the
/// check every feature-specific code path makes before it runs, and one that
/// must not cost a linear search on the message path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct FeatureProfile {
    bits: u128,
}

impl FeatureProfile {
    /// No features at all. What an unnegotiated session has.
    #[must_use]
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Every feature, ignoring revision, capabilities and transport.
    ///
    /// Only for tests that need a profile with a known shape. Production code
    /// goes through [`resolve`].
    #[must_use]
    pub fn all() -> Self {
        let mut profile = Self::empty();
        for feature in Feature::ALL {
            profile.insert(feature);
        }
        profile
    }

    /// Adds a feature.
    #[must_use]
    pub fn with(self, feature: Feature) -> Self {
        Self {
            bits: self.bits | Self::bit_of(feature),
        }
    }

    /// Adds a feature in place.
    pub fn insert(&mut self, feature: Feature) {
        self.bits |= Self::bit_of(feature);
    }

    /// Removes a feature.
    pub fn remove(&mut self, feature: Feature) {
        self.bits &= !Self::bit_of(feature);
    }

    /// Whether the session has the feature.
    #[must_use]
    pub fn allows(self, feature: Feature) -> bool {
        self.bits & Self::bit_of(feature) != 0
    }

    /// How many features the session has.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.bits.count_ones()
    }

    /// Whether the session has nothing.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// The features in the profile, in declaration order.
    pub fn iter(self) -> impl Iterator<Item = Feature> {
        Feature::ALL.into_iter().filter(move |f| self.allows(*f))
    }

    /// The features that are *not* in the profile, in declaration order.
    pub fn missing(self) -> impl Iterator<Item = Feature> {
        Feature::ALL.into_iter().filter(move |f| !self.allows(*f))
    }

    /// The features this session has that `other` does not.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self {
            bits: self.bits & !other.bits,
        }
    }

    /// The features both sessions have.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self {
            bits: self.bits & other.bits,
        }
    }

    /// This profile viewed as a [`FeatureSet`].
    ///
    /// The two types share one bit encoding precisely so that this is a copy of
    /// a `u128` and every comparison between them is a mask test. The
    /// alternative — `FeatureSet::union_of(self.iter())` — walks all
    /// [`Feature::ALL`] and calls [`allows`](Self::allows) for each, and
    /// `allows` resolves its bit by scanning `ALL` for a matching name. That is
    /// quadratic in the feature count, which is invisible when a decision is
    /// made once per message and not when it is made once per primitive on a
    /// `tools/list` of a thousand tools.
    #[must_use]
    pub const fn as_set(self) -> FeatureSet {
        FeatureSet::from_bits(self.bits)
    }

    /// The bit a feature occupies, found by name so a new variant cannot leave
    /// a stale hand-assigned bit behind.
    fn bit_of(feature: Feature) -> u128 {
        let name = feature.as_str();
        let mut i = 0;
        while let Some(candidate) = Feature::ALL.get(i) {
            if str_eq(candidate.as_str(), name) {
                return 1u128 << i;
            }
            i += 1;
        }
        0
    }
}

/// Everything a session's peers and transport tell us, as one argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Negotiation {
    /// The revision both sides agreed on.
    pub version: ProtocolVersion,
    /// What the client advertised.
    pub client_caps: CapabilitySet,
    /// What the server advertised.
    pub server_caps: CapabilitySet,
    /// The transport the session runs over.
    pub transport: TransportKind,
}

impl Negotiation {
    /// The capabilities for a version, on one side, with nothing advertised.
    #[must_use]
    pub const fn bare(version: ProtocolVersion, transport: TransportKind) -> Self {
        Self {
            version,
            client_caps: CapabilitySet::empty(),
            server_caps: CapabilitySet::empty(),
            transport,
        }
    }
}

/// Resolves the features a negotiated session has.
///
/// Pure: the same inputs always give the same profile, and nothing here reads a
/// clock, a socket, or global state. That is what makes the golden snapshot
/// worth having — a profile is a function of four values, so it can be
/// reviewed as a table rather than discovered at runtime.
///
/// A feature is on when *all four* of its row's conditions hold:
///
/// 1. `version` is within the row's revision window.
/// 2. The side that must support it actually declared the required capability,
///    for the side that owns it.
/// 3. The transport, if the row names one, is defined for the version and
///    matches.
/// 4. The capability the row needs is one the version's schema defines, which
///    stops a newer peer's flag from switching on a feature an older revision
///    cannot express.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{
///     Capability, CapabilitySet, Feature, Negotiation, ProtocolVersion,
///     TransportKind, resolve,
/// };
///
/// // 2024-11-05 over stdio, with everything that revision defines advertised.
/// let version = ProtocolVersion::V2024_11_05;
/// let all = CapabilitySet::all_for(version);
/// let profile = resolve(Negotiation {
///     version,
///     client_caps: all,
///     server_caps: all,
///     transport: TransportKind::Stdio,
/// });
/// assert!(profile.allows(Feature::Tools));
/// assert!(!profile.allows(Feature::Elicitation), "added in 2025-06-18");
/// ```
#[must_use]
pub fn resolve(negotiation: Negotiation) -> FeatureProfile {
    let Negotiation {
        version,
        client_caps,
        server_caps,
        transport,
    } = negotiation;

    // An unknown revision cannot express anything, so the whole profile is
    // empty before the table is even consulted. Same fail-secure posture as
    // `transport_allowed`.
    if !version.is_known() {
        return FeatureProfile::empty();
    }

    let mut profile = FeatureProfile::empty();
    for spec in FEATURES {
        if !spec.defined_in(version) {
            continue;
        }
        if let Some(required) = spec.transport
            && required != transport
        {
            continue;
        }
        // A transport-gated row also needs the transport to exist for the
        // revision, which `transport_allowed` is the authority on.
        if spec.transport.is_some() && !transport_allowed(version, transport) {
            continue;
        }
        if !supported_by(spec.side, spec.client_cap, client_caps, version) {
            continue;
        }
        if !supported_by(spec.side, spec.server_cap, server_caps, version) {
            continue;
        }
        profile.insert(spec.feature);
    }
    profile
}

/// Whether the owner of a capability has advertised it on a version that
/// defines it.
fn supported_by(
    side: Side,
    required: Option<Capability>,
    caps: CapabilitySet,
    version: ProtocolVersion,
) -> bool {
    let Some(required) = required else {
        // A row with no capability for this side is on unless the side does
        // not exist, which cannot happen: both sides always do.
        return true;
    };
    // The capability has to be one the side can declare in the first place,
    // and one the version's schema has a field for.
    let owner_declares = match side {
        Side::Client => required.side().includes_client(),
        Side::Server => required.side().includes_server(),
        Side::Both => true,
    };
    owner_declares && required.defined_in(version) && caps.contains(required)
}

/// The downgrades a caller will accept when a feature is unavailable.
///
/// Separate from [`FeatureProfile`] on purpose: a profile is a fact, and a
/// policy is a decision. Keeping them apart means "what can this session do"
/// and "what will this caller settle for" are separately testable, and a
/// session cannot quietly relax its own requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct DegradePolicy {
    bits: u128,
}

impl DegradePolicy {
    /// Nothing may be downgraded. The default, and the fail-secure one.
    #[must_use]
    pub const fn strict() -> Self {
        Self { bits: 0 }
    }

    /// Every feature may be downgraded. For a prober that wants to know what
    /// a peer will tolerate, never for a server serving a client.
    #[must_use]
    pub fn permissive() -> Self {
        Self::all()
    }

    /// Every feature.
    #[must_use]
    pub fn all() -> Self {
        let mut policy = Self::strict();
        for feature in Feature::ALL {
            policy.insert(feature);
        }
        policy
    }

    /// Allows one feature to be downgraded.
    #[must_use]
    pub fn with(self, feature: Feature) -> Self {
        Self {
            bits: self.bits | FeatureProfile::bit_of(feature),
        }
    }

    /// Adds a feature in place.
    pub fn insert(&mut self, feature: Feature) {
        self.bits |= FeatureProfile::bit_of(feature);
    }

    /// Whether this feature may be downgraded.
    #[must_use]
    pub fn allows(self, feature: Feature) -> bool {
        self.bits & FeatureProfile::bit_of(feature) != 0
    }

    /// Whether the session's profile is acceptable under this policy.
    ///
    /// `True` when every feature the session *lacks* is one the policy allows
    /// to be missing. A strict policy therefore accepts only a profile with
    /// everything, which is the point: the caller has to opt in to each
    /// individual loss.
    #[must_use]
    pub fn accepts(self, profile: FeatureProfile) -> bool {
        profile.missing().all(|feature| self.allows(feature))
    }
}

#[cfg(test)]
mod tests {
    // Test code unwraps deliberately: the strict profile exists to keep
    // production paths fallible, and a test that cannot unwrap cannot assert.
    // The messages name the failing precondition, which is the useful part.
    #![expect(
        clippy::panic,
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
    )]

    use super::{DegradePolicy, FEATURES, Feature, FeatureProfile, Negotiation, resolve};
    use crate::domain::{Capability, CapabilitySet, ProtocolVersion, TransportKind};
    use std::collections::BTreeSet;
    fn everything_on(version: ProtocolVersion, transport: TransportKind) -> FeatureProfile {
        let caps = CapabilitySet::all_for(version);
        resolve(Negotiation {
            version,
            client_caps: caps,
            server_caps: caps,
            transport,
        })
    }

    fn nothing_on(version: ProtocolVersion, transport: TransportKind) -> FeatureProfile {
        resolve(Negotiation::bare(version, transport))
    }

    #[test]
    fn the_table_covers_every_feature_exactly_once() {
        for feature in Feature::ALL {
            assert_eq!(
                FEATURES.iter().filter(|s| s.feature == feature).count(),
                1,
                "{feature} has a duplicate row"
            );
        }
        assert_eq!(FEATURES.len(), Feature::ALL.len());
    }

    /// A removal must be attributed to a revision strictly after the one that
    /// added it, and must name known revisions. This is the half of "every row
    /// cites the spec" that can be checked mechanically.
    #[test]
    fn every_row_is_cited_and_ordered() {
        for spec in FEATURES {
            assert!(
                spec.first.is_known(),
                "{} cites {}",
                spec.feature,
                spec.first
            );
            if let Some(last) = spec.last {
                assert!(last.is_known(), "{} cites {}", spec.feature, last);
                assert!(
                    spec.first < last,
                    "{} is removed by {last}, which does not follow {}",
                    spec.feature,
                    spec.first
                );
            }
        }
    }

    /// Every capability a row requires has to be one the row's *own* revision
    /// window defines. A row that requires a flag its first revision cannot
    /// express could never turn on, which is a dead row rather than a wrong
    /// one — but it is always a mistake.
    #[test]
    fn no_row_requires_a_capability_its_revisions_lack() {
        for spec in FEATURES {
            for capability in [spec.client_cap, spec.server_cap].into_iter().flatten() {
                for version in ProtocolVersion::KNOWN {
                    if spec.defined_in(version) {
                        assert!(
                            capability.defined_in(version),
                            "{capability} is required by {} but is not defined in {version}",
                            spec.feature
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_feature_fits_in_the_bitset_with_a_distinct_bit() {
        let mut seen = BTreeSet::new();
        let profile = FeatureProfile::all();
        assert_eq!(profile.len() as usize, Feature::ALL.len());
        for feature in Feature::ALL {
            assert!(profile.allows(feature), "{feature} has no bit");
            let alone = FeatureProfile::empty().with(feature);
            assert_eq!(alone.len(), 1, "{feature} is not alone in its bit");
            assert!(seen.insert(alone.iter().next()), "{feature} shares a bit");
        }
    }

    #[test]
    fn the_golden_profile_grows_across_revisions() {
        let counts: Vec<(ProtocolVersion, u32)> = ProtocolVersion::KNOWN
            .into_iter()
            .map(|version| (version, everything_on(version, TransportKind::Stdio).len()))
            .collect();
        // Nothing before `2025-03-26`, and the removals in `2026-07-28` offset
        // some of that growth, so only the broad shape is asserted here. The
        // exact numbers are the golden file's job.
        let (Some(first), Some(last)) = (counts.first(), counts.last()) else {
            panic!("no revisions were counted");
        };
        assert!(first.1 < last.1, "{counts:?}");
        for pair in counts.windows(2) {
            let (Some(earlier), Some(later)) = (pair.first(), pair.last()) else {
                continue;
            };
            assert!(
                later.1 >= earlier.1 - 2,
                "{earlier:?} to {later:?} lost more than the removals explain"
            );
        }
    }

    /// A peer that advertises nothing gets the capability-free protocol-level
    /// features and nothing else. `ping` and `_meta` are unconditional, so the
    /// profile is *not* empty — what matters is that every gated feature is off.
    #[test]
    fn a_peer_that_advertises_nothing_gets_no_gated_features() {
        for version in ProtocolVersion::KNOWN {
            for transport in TransportKind::ALL {
                let profile = nothing_on(version, transport);
                for spec in FEATURES {
                    let gated = spec.client_cap.is_some() || spec.server_cap.is_some();
                    if gated {
                        assert!(
                            !profile.allows(spec.feature),
                            "{version} over {transport}: {} is gated but is on",
                            spec.feature
                        );
                    }
                }
            }
        }
        // And the ungated ones that are in range really are on, so the loop
        // above is not passing because `resolve` returns nothing at all.
        let old = nothing_on(ProtocolVersion::V2024_11_05, TransportKind::Stdio);
        assert!(old.allows(Feature::Ping));
        assert!(
            !old.allows(Feature::MetaExtensions),
            "`_meta` starts in 2025-06-18"
        );
        let mid = nothing_on(ProtocolVersion::V2025_06_18, TransportKind::Stdio);
        assert!(mid.allows(Feature::MetaExtensions));
        assert!(mid.allows(Feature::McpProtocolVersionHeader));
    }

    /// The fail-secure property, at the top of the stack this time.
    #[test]
    fn an_unknown_revision_yields_nothing() {
        let future = ProtocolVersion::Unknown("2027-01-01".parse().unwrap());
        assert!(everything_on(future, TransportKind::Stdio).is_empty());
    }

    #[test]
    fn batching_exists_for_exactly_one_revision() {
        for version in ProtocolVersion::KNOWN {
            let on = everything_on(version, TransportKind::Stdio).allows(Feature::JsonRpcBatching);
            assert_eq!(
                on,
                version == ProtocolVersion::V2025_03_26,
                "batching in {version}"
            );
        }
    }

    #[test]
    fn the_2026_removals_are_actually_removed() {
        for version in ProtocolVersion::KNOWN {
            let profile = everything_on(version, TransportKind::Stdio);
            let newest = version == ProtocolVersion::V2026_07_28;
            assert_eq!(profile.allows(Feature::Ping), !newest, "ping in {version}");
            assert_eq!(
                profile.allows(Feature::RootsListChanged),
                !newest,
                "roots.listChanged in {version}"
            );
            // Tasks came in 2025-11-25 and left in 2026-07-28.
            let expected = version == ProtocolVersion::V2025_11_25;
            assert_eq!(
                profile.allows(Feature::Tasks),
                expected,
                "tasks in {version}"
            );
        }
    }

    #[test]
    fn a_capability_nobody_advertised_keeps_its_feature_off() {
        let version = ProtocolVersion::V2026_07_28;
        let mut server = CapabilitySet::empty();
        server.insert(Capability::Tools);
        let profile = resolve(Negotiation {
            version,
            client_caps: CapabilitySet::all_for(version),
            server_caps: server,
            transport: TransportKind::Stdio,
        });
        // Core tools work, because the server said it has them.
        assert!(profile.allows(Feature::Tools));
        // `completions` needs the server to advertise it.
        assert!(!profile.allows(Feature::Completions));
        assert!(!profile.allows(Feature::ToolsListChanged));
        // The server said nothing about resources, so it has none.
        assert!(!profile.allows(Feature::Resources));
        assert!(!profile.allows(Feature::Prompts));
    }

    #[test]
    fn a_dependent_capability_does_not_imply_its_parent() {
        // A peer that sends `elicitation.url` without `elicitation` is
        // malformed, and the resolver must not quietly promote it: the row
        // names `elicitation.url` and `elicitation.url` is what it checks.
        let version = ProtocolVersion::V2025_11_25;
        let client = CapabilitySet::empty().with(Capability::ElicitationUrl);
        let caps = client;
        let profile = resolve(Negotiation {
            version,
            client_caps: caps,
            server_caps: CapabilitySet::all_for(version),
            transport: TransportKind::Stdio,
        });
        assert!(profile.allows(Feature::ElicitationUrl));
        assert!(!profile.allows(Feature::Elicitation));
        assert!(!profile.allows(Feature::ElicitationForm));
    }

    #[test]
    fn transport_gated_features_follow_the_transport() {
        // Streamable HTTP is not defined before 2025-03-26 and not on stdio.
        let old = everything_on(ProtocolVersion::V2024_11_05, TransportKind::Stdio);
        assert!(!old.allows(Feature::StreamableHttp));

        let new = everything_on(ProtocolVersion::V2026_07_28, TransportKind::StreamableHttp);
        assert!(new.allows(Feature::StreamableHttp));

        let on_stdio = everything_on(ProtocolVersion::V2026_07_28, TransportKind::Stdio);
        assert!(!on_stdio.allows(Feature::StreamableHttp));
    }

    #[test]
    fn legacy_sse_survives_only_on_2024_11_05() {
        for version in ProtocolVersion::KNOWN {
            let profile = everything_on(version, TransportKind::LegacySse);
            let expected = version == ProtocolVersion::V2024_11_05;
            assert_eq!(
                profile.allows(Feature::LegacySse),
                expected,
                "legacy sse in {version}"
            );
        }
    }

    #[test]
    fn profile_set_operations_behave() {
        let full = FeatureProfile::all();
        let empty = FeatureProfile::empty();
        assert!(empty.difference(full).is_empty());
        assert_eq!(full.difference(empty), full);
        assert_eq!(full.intersection(full), full);
        assert!(full.intersection(empty).is_empty());

        let one = FeatureProfile::empty().with(Feature::Tools);
        assert_eq!(one.difference(full).len(), 0);
        assert_eq!(full.difference(one).len() as usize, Feature::ALL.len() - 1);
    }

    #[test]
    fn insert_and_remove_work_in_place() {
        let mut profile = FeatureProfile::empty();
        assert!(!profile.allows(Feature::Tools));
        profile.insert(Feature::Tools);
        assert!(profile.allows(Feature::Tools));
        profile.remove(Feature::Tools);
        assert!(!profile.allows(Feature::Tools));
        assert!(profile.is_empty());
    }

    #[test]
    fn a_strict_policy_accepts_only_a_complete_profile() {
        let strict = DegradePolicy::strict();
        let full = everything_on(ProtocolVersion::V2026_07_28, TransportKind::Stdio);
        // The full profile is missing the features that revision removed, so
        // even it is not accepted by a strict policy — which is correct: a
        // strict policy means "no losses at all".
        assert!(!strict.accepts(full));
        assert!(strict.accepts(FeatureProfile::all()));

        // One tolerated loss is enough to make the full profile acceptable.
        let mut lenient = DegradePolicy::strict();
        for feature in full.missing() {
            lenient.insert(feature);
        }
        assert!(lenient.accepts(full));
    }

    #[test]
    fn a_permissive_policy_accepts_anything() {
        let permissive = DegradePolicy::permissive();
        assert!(permissive.accepts(FeatureProfile::empty()));
        assert!(permissive.accepts(FeatureProfile::all()));
        assert!(permissive.allows(Feature::Ping));
    }

    #[test]
    fn a_policy_tolerating_one_feature_still_rejects_another() {
        let policy = DegradePolicy::strict().with(Feature::Ping);
        assert!(policy.allows(Feature::Ping));
        assert!(!policy.allows(Feature::Tools));
        // Loses both features, so one tolerance is not enough.
        let lossy = FeatureProfile::empty()
            .with(Feature::Tools)
            .with(Feature::Resources);
        assert!(!policy.accepts(lossy));
    }
}
