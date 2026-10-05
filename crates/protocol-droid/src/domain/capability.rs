//! `Capability` — the flags a peer advertises, and which revisions define them.
//!
//! # Why this table is not written from memory
//!
//! Capabilities are the input to [`resolve`](crate::domain::resolve) and they
//! change shape silently between revisions: `tasks` appears on both sides in
//! `2025-11-25` and is gone from both in `2026-07-28`, `roots.listChanged`
//! disappears when `notifications/roots/list_changed` is removed, and
//! `elicitation` grows `form` and `url` sub-flags. A table written from
//! recollection would be wrong in at least one row and the error would surface
//! as a feature that silently fails to enable.
//!
//! So the table is hardcoded — it has to be, it is the domain's own model — but
//! it is **checked against the vendored `schema.json` at test time**. See
//! [`tests::the_table_matches_every_vendored_schema`], which walks the real
//! capability tree of all six revisions and fails if the two disagree in either
//! direction. That makes the table self-verifying: a future revision that adds
//! or drops a capability fails the build rather than quietly getting a
//! nonsensical feature profile.
//!
//! # Capability is not method
//!
//! The two come apart, and the clearest case is `resources.subscribe`:
//! `2026-07-28` replaces the `resources/subscribe` and `resources/unsubscribe`
//! *methods* with `subscriptions/listen`, and still defines
//! `resources.subscribe` as a server *capability*. A capability records that a
//! peer opted into a kind of behaviour; the methods that implement it are
//! T08's concern. Conflating them would remove the capability in
//! `2026-07-28` and lose the information that the peer is still subscribed.

use crate::domain::ProtocolVersion;

/// Which side of the connection a capability is declared on.
///
/// A capability that is not declared by the side that would provide it cannot
/// be enabled, and keeping the two apart is what makes that checkable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// Declared by the client, describing what the client can do.
    Client,
    /// Declared by the server, describing what the server can do.
    Server,
    /// Declared by both, with the same meaning.
    Both,
}

impl Side {
    /// Whether a client may declare this side.
    #[must_use]
    pub const fn includes_client(self) -> bool {
        matches!(self, Self::Client | Self::Both)
    }

    /// Whether a server may declare this side.
    #[must_use]
    pub const fn includes_server(self) -> bool {
        matches!(self, Self::Server | Self::Both)
    }
}

/// A capability name as the specification spells it.
///
/// Fieldless, and one variant per leaf name in the capability trees. The
/// container names (`roots`, `tasks`, …) are variants too: a peer that
/// advertises `roots` has advertised that it has roots, and a feature gate on
/// "the peer has roots at all" is a real question that
/// [`FeatureProfile`](crate::domain::FeatureProfile) has to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// Client and server `experimental` bags. Present in every revision.
    Experimental,
    /// `extensions`, new in `2026-07-28` on both sides.
    Extensions,
    /// Client `roots`. Deprecated in `2026-07-28` but still defined.
    Roots,
    /// Client `roots.listChanged`, removed in `2026-07-28` along with the
    /// `notifications/roots/list_changed` method.
    RootsListChanged,
    /// Client `sampling`. Deprecated in `2026-07-28` but still defined.
    Sampling,
    /// Client `sampling.context`, added `2025-11-25`.
    SamplingContext,
    /// Client `sampling.tools`, added `2025-11-25` (SEP-1577).
    SamplingTools,
    /// Client `elicitation`, added `2025-06-18`.
    Elicitation,
    /// Client `elicitation.form`, added `2025-11-25` (SEP-1330).
    ElicitationForm,
    /// Client `elicitation.url`, added `2025-11-25` (SEP-1036).
    ElicitationUrl,
    /// Server `logging`. Deprecated in `2026-07-28`; `logging/setLevel` is
    /// removed, log level moves to `_meta`.
    Logging,
    /// Server `prompts`, every revision.
    Prompts,
    /// Server `prompts.listChanged`, every revision.
    PromptsListChanged,
    /// Server `resources`, every revision.
    Resources,
    /// Server `resources.listChanged`, every revision.
    ResourcesListChanged,
    /// Server `resources.subscribe`. The *methods* behind it are replaced in
    /// `2026-07-28`; the capability is not.
    ResourcesSubscribe,
    /// Server `tools`, every revision.
    Tools,
    /// Server `tools.listChanged`, every revision.
    ToolsListChanged,
    /// Server `completions`, added `2025-03-26`.
    Completions,
    /// `tasks`, added `2025-11-25` (SEP-1686) on both sides and **removed**
    /// from both in `2026-07-28`, where it moves to the
    /// `io.modelcontextprotocol/tasks` extension (SEP-2663).
    Tasks,
    /// Client and server `tasks.cancel`, `2025-11-25` only.
    TasksCancel,
    /// Client and server `tasks.list`, `2025-11-25` only. The method was
    /// removed in `2026-07-28` along with the capability.
    TasksList,
    /// Client and server `tasks.requests`, `2025-11-25` only.
    TasksRequests,
    /// Client `tasks.requests.elicitation`, `2025-11-25` only.
    TasksRequestsElicitation,
    /// Client `tasks.requests.elicitation.create`, `2025-11-25` only.
    TasksRequestsElicitationCreate,
    /// Client `tasks.requests.sampling`, `2025-11-25` only.
    TasksRequestsSampling,
    /// Client `tasks.requests.sampling.createMessage`, `2025-11-25` only.
    TasksRequestsSamplingCreateMessage,
    /// Server `tasks.requests.tools`, `2025-11-25` only.
    TasksRequestsTools,
    /// Server `tasks.requests.tools.call`, `2025-11-25` only.
    TasksRequestsToolsCall,
}

impl Capability {
    /// Every capability, in declaration order.
    pub const ALL: [Self; 29] = [
        Self::Experimental,
        Self::Extensions,
        Self::Roots,
        Self::RootsListChanged,
        Self::Sampling,
        Self::SamplingContext,
        Self::SamplingTools,
        Self::Elicitation,
        Self::ElicitationForm,
        Self::ElicitationUrl,
        Self::Logging,
        Self::Prompts,
        Self::PromptsListChanged,
        Self::Resources,
        Self::ResourcesListChanged,
        Self::ResourcesSubscribe,
        Self::Tools,
        Self::ToolsListChanged,
        Self::Completions,
        Self::Tasks,
        Self::TasksCancel,
        Self::TasksList,
        Self::TasksRequests,
        Self::TasksRequestsElicitation,
        Self::TasksRequestsElicitationCreate,
        Self::TasksRequestsSampling,
        Self::TasksRequestsSamplingCreateMessage,
        Self::TasksRequestsTools,
        Self::TasksRequestsToolsCall,
    ];

    /// The name the specification uses, dot-separated.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Experimental => "experimental",
            Self::Extensions => "extensions",
            Self::Roots => "roots",
            Self::RootsListChanged => "roots.listChanged",
            Self::Sampling => "sampling",
            Self::SamplingContext => "sampling.context",
            Self::SamplingTools => "sampling.tools",
            Self::Elicitation => "elicitation",
            Self::ElicitationForm => "elicitation.form",
            Self::ElicitationUrl => "elicitation.url",
            Self::Logging => "logging",
            Self::Prompts => "prompts",
            Self::PromptsListChanged => "prompts.listChanged",
            Self::Resources => "resources",
            Self::ResourcesListChanged => "resources.listChanged",
            Self::ResourcesSubscribe => "resources.subscribe",
            Self::Tools => "tools",
            Self::ToolsListChanged => "tools.listChanged",
            Self::Completions => "completions",
            Self::Tasks => "tasks",
            Self::TasksCancel => "tasks.cancel",
            Self::TasksList => "tasks.list",
            Self::TasksRequests => "tasks.requests",
            Self::TasksRequestsElicitation => "tasks.requests.elicitation",
            Self::TasksRequestsElicitationCreate => "tasks.requests.elicitation.create",
            Self::TasksRequestsSampling => "tasks.requests.sampling",
            Self::TasksRequestsSamplingCreateMessage => "tasks.requests.sampling.createMessage",
            Self::TasksRequestsTools => "tasks.requests.tools",
            Self::TasksRequestsToolsCall => "tasks.requests.tools.call",
        }
    }

    /// Which side declares it.
    #[must_use]
    pub const fn side(self) -> Side {
        match self {
            Self::Experimental
            | Self::Extensions
            | Self::Tasks
            | Self::TasksCancel
            | Self::TasksList
            | Self::TasksRequests => Side::Both,
            Self::Roots
            | Self::RootsListChanged
            | Self::Sampling
            | Self::SamplingContext
            | Self::SamplingTools
            | Self::Elicitation
            | Self::ElicitationForm
            | Self::ElicitationUrl
            | Self::TasksRequestsElicitation
            | Self::TasksRequestsElicitationCreate
            | Self::TasksRequestsSampling
            | Self::TasksRequestsSamplingCreateMessage => Side::Client,
            Self::Logging
            | Self::Prompts
            | Self::PromptsListChanged
            | Self::Resources
            | Self::ResourcesListChanged
            | Self::ResourcesSubscribe
            | Self::Tools
            | Self::ToolsListChanged
            | Self::Completions
            | Self::TasksRequestsTools
            | Self::TasksRequestsToolsCall => Side::Server,
        }
    }

    /// The bit this capability occupies in a [`CapabilitySet`].
    ///
    /// The index is the position in [`Capability::ALL`], found by name rather
    /// than written by hand, so adding a variant cannot leave a stale
    /// hand-assigned bit behind. Asserted to be a distinct in-range power of
    /// two by [`tests::every_capability_fits_in_the_bitset`].
    ///
    /// Not `const`: the `indexing_slicing` lint is denied workspace-wide, and
    /// `slice::get` — the non-panicking form it demands — is not a
    /// const-callable function on 1.98. The two cannot both be satisfied, and
    /// an unchecked index in a `const` fn is not worth suppressing the lint
    /// for: nothing here is evaluated at compile time.
    #[must_use]
    pub fn bit(self) -> u128 {
        let name = self.as_str();
        let mut i = 0;
        while let Some(candidate) = Self::ALL.get(i) {
            if str_eq(candidate.as_str(), name) {
                return 1u128 << i;
            }
            i += 1;
        }
        0
    }

    /// Whether the given revision defines this capability at all.
    ///
    /// Not the same question as whether a particular peer advertises it: a
    /// revision either has the name in its schema or it does not, and a peer on
    /// that revision either sends it or does not.
    #[must_use]
    pub fn defined_in(self, version: ProtocolVersion) -> bool {
        defined_in(self, version)
    }
}

/// A capability and the revisions whose schemas define it.
///
/// The table is the domain's model; the test
/// [`tests::the_table_matches_every_vendored_schema`] is what keeps it honest
/// against the vendored bytes. Citations are to the changelog of the revision
/// that introduced the capability, which is the only place the fact is written
/// down.
pub const CAPABILITY_TABLE: &[(Capability, &[ProtocolVersion])] = &[
    (
        Capability::Experimental,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        // SEP-2575, `2026-07-28`: `extensions` replaces per-revision ad-hoc
        // extension flags with a declared bag.
        Capability::Extensions,
        &[ProtocolVersion::V2026_07_28],
    ),
    (
        // Present from the first revision. Deprecated by SEP-2577 in
        // `2026-07-28`, but still in the schema.
        Capability::Roots,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        // `roots.listChanged` goes with `notifications/roots/list_changed`,
        // removed by SEP-2575 in `2026-07-28`.
        Capability::RootsListChanged,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
        ],
    ),
    (
        // Present from the first revision; deprecated by SEP-2577 in
        // `2026-07-28`.
        Capability::Sampling,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        // SEP-1577, `2025-11-25`.
        Capability::SamplingContext,
        &[ProtocolVersion::V2025_11_25, ProtocolVersion::V2026_07_28],
    ),
    (
        // SEP-1577, `2025-11-25`: tool calling inside sampling.
        Capability::SamplingTools,
        &[ProtocolVersion::V2025_11_25, ProtocolVersion::V2026_07_28],
    ),
    (
        // `2025-06-18` major change 6, PR #382.
        Capability::Elicitation,
        &[
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        // SEP-1330, `2025-11-25`: titled/untitled, single- and multi-select.
        Capability::ElicitationForm,
        &[ProtocolVersion::V2025_11_25, ProtocolVersion::V2026_07_28],
    ),
    (
        // SEP-1036, `2025-11-25`. The `notifications/elicitation/complete`
        // notification and the `elicitationId` field behind it are removed in
        // `2026-07-28`, but the capability itself survives.
        Capability::ElicitationUrl,
        &[ProtocolVersion::V2025_11_25, ProtocolVersion::V2026_07_28],
    ),
    (
        // Present from the first revision; deprecated by SEP-2577 in
        // `2026-07-28`, which also removes `logging/setLevel`.
        Capability::Logging,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        Capability::Prompts,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        Capability::PromptsListChanged,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        Capability::Resources,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        Capability::ResourcesListChanged,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        // Defined in every revision including `2026-07-28`, where the
        // `resources/subscribe` and `resources/unsubscribe` methods are
        // replaced by `subscriptions/listen` (SEP-2575). The capability
        // records that the peer is subscribed; the method is T08's problem.
        Capability::ResourcesSubscribe,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        Capability::Tools,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        Capability::ToolsListChanged,
        &[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        // `2025-03-26`: "Added `completions` capability to explicitly indicate
        // support for argument autocompletion suggestions".
        Capability::Completions,
        &[
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ],
    ),
    (
        // SEP-1686 adds it in `2025-11-25`; SEP-2663 moves it out of the core
        // protocol into the `io.modelcontextprotocol/tasks` extension in
        // `2026-07-28`, so it is defined in exactly one revision.
        Capability::Tasks,
        &[ProtocolVersion::V2025_11_25],
    ),
    (Capability::TasksCancel, &[ProtocolVersion::V2025_11_25]),
    (Capability::TasksList, &[ProtocolVersion::V2025_11_25]),
    (Capability::TasksRequests, &[ProtocolVersion::V2025_11_25]),
    (
        Capability::TasksRequestsElicitation,
        &[ProtocolVersion::V2025_11_25],
    ),
    (
        Capability::TasksRequestsElicitationCreate,
        &[ProtocolVersion::V2025_11_25],
    ),
    (
        Capability::TasksRequestsSampling,
        &[ProtocolVersion::V2025_11_25],
    ),
    (
        Capability::TasksRequestsSamplingCreateMessage,
        &[ProtocolVersion::V2025_11_25],
    ),
    (
        Capability::TasksRequestsTools,
        &[ProtocolVersion::V2025_11_25],
    ),
    (
        Capability::TasksRequestsToolsCall,
        &[ProtocolVersion::V2025_11_25],
    ),
];

/// Whether `version`'s schema defines `capability`.
///
/// An unknown revision defines nothing, for the same reason
/// [`transport_allowed`](crate::domain::transport_allowed) denies: this build
/// has no wire types for it, so no flag in it can be trusted.
///
/// Not `const`: `PartialEq` on an enum is not a const-callable operator on
/// 1.98, so the table walk cannot be one. The cost is a scan of at most
/// [`CAPABILITY_TABLE`] rows, and this is called from
/// [`resolve`](crate::domain::resolve) and the tests rather than per message;
/// [`CapabilitySet::restricted_to`] is the form that runs in bulk.
#[must_use]
pub fn defined_in(capability: Capability, version: ProtocolVersion) -> bool {
    if matches!(version, ProtocolVersion::Unknown(_)) {
        return false;
    }
    CAPABILITY_TABLE
        .iter()
        .find(|(candidate, _)| *candidate == capability)
        .is_some_and(|(_, versions)| versions.contains(&version))
}

impl core::fmt::Display for Capability {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A set of advertised capabilities.
///
/// A `u128` bitset, so [`contains`](Self::contains) is a mask and a test rather
/// than a search. This matters because [`resolve`](crate::domain::resolve) asks
/// roughly thirty questions of two sets, and a session asks
/// [`FeatureProfile::allows`](crate::domain::FeatureProfile::allows) on every
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct CapabilitySet {
    bits: u128,
}

impl CapabilitySet {
    /// The empty set, which is what a peer that advertises nothing gets.
    #[must_use]
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Every capability, on both sides. A superset of anything real, and
    /// mostly useful in tests and in the "what could this peer possibly do"
    /// query.
    #[must_use]
    pub fn all() -> Self {
        let mut set = Self::empty();
        for capability in Capability::ALL {
            set.insert(capability);
        }
        set
    }

    /// Every capability `version` defines, on both sides.
    ///
    /// The "all capabilities on" case for a golden test, scoped to one revision
    /// so the result is something a peer could actually have sent.
    #[must_use]
    pub fn all_for(version: ProtocolVersion) -> Self {
        let mut set = Self::empty();
        for (capability, versions) in CAPABILITY_TABLE {
            if versions.contains(&version) {
                set.insert(*capability);
            }
        }
        set
    }

    /// Adds a capability, returning the new set.
    ///
    /// By value rather than `&mut self` so that building a set in an
    /// expression does not need a mutable binding, which is what every caller
    /// ends up writing.
    #[must_use]
    pub fn with(self, capability: Capability) -> Self {
        Self {
            bits: self.bits | capability.bit(),
        }
    }

    /// Adds a capability in place.
    pub fn insert(&mut self, capability: Capability) {
        self.bits |= capability.bit();
    }

    /// Whether the capability is in the set.
    #[must_use]
    pub fn contains(self, capability: Capability) -> bool {
        self.bits & capability.bit() != 0
    }

    /// Whether the set holds nothing.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// How many capabilities the set holds.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.bits.count_ones()
    }

    /// The capabilities in the set, in declaration order.
    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL
            .into_iter()
            .filter(move |c| self.contains(*c))
    }

    /// Drops every capability `version` does not define.
    ///
    /// A peer cannot advertise a flag its own revision's schema has no field
    /// for, and a flag that survives the round trip from a newer peer into an
    /// older profile is exactly the "confident wrong" the crate exists to
    /// avoid. An unknown revision therefore reduces the set to empty.
    #[must_use]
    pub fn restricted_to(self, version: ProtocolVersion) -> Self {
        let mut out = Self::empty();
        for capability in self.iter() {
            if capability.defined_in(version) {
                out.insert(capability);
            }
        }
        out
    }
}

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

/// How one server capability appears in a `ServerCapabilities` object.
///
/// A *key path* rather than a key and an optional child, because the nesting
/// is not two deep everywhere: `tasks.requests.tools.call` is three, and a
/// `(key, child)` row would have to lie about it or leave it unrepresentable.
///
/// Rows are only ever walked from the outside in, so a parent with no child of
/// its own still emits its own object — `tools` is present exactly when the
/// server has tools, and that presence is the whole signal.
///
/// Data rather than a `match` for the same reason [`FEATURES`] is data: a
/// capability added to the domain without a row here would be dropped
/// silently, and [`capability_wire_is_total`] is what keeps the two in step.
///
/// [`FEATURES`]: crate::domain::FEATURES
/// [`capability_wire_is_total`]: crate::domain::capability
pub const SERVER_CAPABILITY_WIRE: &[(Capability, &[&str])] = &[
    (Capability::Experimental, &["experimental"]),
    (Capability::Tools, &["tools"]),
    (Capability::ToolsListChanged, &["tools", "listChanged"]),
    (Capability::Resources, &["resources"]),
    (Capability::ResourcesSubscribe, &["resources", "subscribe"]),
    (Capability::ResourcesListChanged, &["resources", "listChanged"]),
    (Capability::Prompts, &["prompts"]),
    (Capability::PromptsListChanged, &["prompts", "listChanged"]),
    (Capability::Completions, &["completions"]),
    (Capability::Logging, &["logging"]),
    (Capability::Tasks, &["tasks"]),
];

/// How one client capability appears in a `ClientCapabilities` object.
///
/// The mirror of [`SERVER_CAPABILITY_WIRE`], and in the same place for the same
/// reason: a client that advertises a capability the reader cannot recognise
/// loses it silently, which is a feature that appears to work right up until
/// the peer uses it.
///
/// Deliberately a *separate* table rather than one table with a side column.
/// The two sides do not have the same shapes — `roots` is client-only,
/// `tools` is server-only, and `tasks` nests differently under each — and one
/// table with a side column would carry a discriminator through both the
/// renderer and the reader to express a difference that the data already has.
pub const CLIENT_CAPABILITY_WIRE: &[(Capability, &[&str])] = &[
    (Capability::Experimental, &["experimental"]),
    (Capability::Extensions, &["extensions"]),
    (Capability::Roots, &["roots"]),
    (Capability::RootsListChanged, &["roots", "listChanged"]),
    (Capability::Sampling, &["sampling"]),
    (Capability::SamplingContext, &["sampling", "context"]),
    (Capability::SamplingTools, &["sampling", "tools"]),
    (Capability::Elicitation, &["elicitation"]),
    (Capability::ElicitationForm, &["elicitation", "form"]),
    (Capability::ElicitationUrl, &["elicitation", "url"]),
    (Capability::Tasks, &["tasks"]),
    (Capability::TasksCancel, &["tasks", "cancel"]),
    (Capability::TasksList, &["tasks", "list"]),
    (Capability::TasksRequests, &["tasks", "requests"]),
    (
        Capability::TasksRequestsElicitation,
        &["tasks", "requests", "elicitation"],
    ),
    (
        Capability::TasksRequestsElicitationCreate,
        &["tasks", "requests", "elicitation", "create"],
    ),
    (
        Capability::TasksRequestsSampling,
        &["tasks", "requests", "sampling"],
    ),
    (
        Capability::TasksRequestsSamplingCreateMessage,
        &["tasks", "requests", "sampling", "createMessage"],
    ),
];

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

    use super::{CAPABILITY_TABLE, Capability, CapabilitySet, Side, defined_in};
    use crate::domain::ProtocolVersion;
    use std::collections::BTreeSet;

    #[test]
    fn every_capability_fits_in_the_bitset() {
        let mut seen = BTreeSet::new();
        for capability in Capability::ALL {
            let bit = capability.bit();
            assert!(
                bit != 0 && bit.is_power_of_two(),
                "{} has a bad bit {bit:#x}",
                capability.as_str()
            );
            assert!(bit < 1u128 << 100, "{} overflows", capability.as_str());
            assert!(seen.insert(bit), "{} duplicates a bit", capability.as_str());
        }
    }

    #[test]
    fn names_are_unique_and_dotted_paths_are_well_formed() {
        let mut names = BTreeSet::new();
        for capability in Capability::ALL {
            assert!(
                names.insert(capability.as_str()),
                "duplicate name {}",
                capability.as_str()
            );
            assert!(!capability.as_str().is_empty());
            assert!(!capability.as_str().contains(' '));
        }
        // Every dotted name has a container variant that is also a capability,
        // and the container's side admits the child's: `tasks.requests` is
        // declared on both sides while `tasks.requests.tools` is server-only.
        for capability in Capability::ALL {
            if let Some((container, _)) = capability.as_str().split_once('.') {
                let Some(found) = Capability::ALL.iter().find(|c| c.as_str() == container) else {
                    panic!("{container} has no variant");
                };
                let (container_side, child_side) = (found.side(), capability.side());
                let admitted = match child_side {
                    Side::Client => container_side.includes_client(),
                    Side::Server => container_side.includes_server(),
                    // A `Both` child needs a `Both` container: neither side on
                    // its own declares a capability the other side also has.
                    Side::Both => container_side == Side::Both,
                };
                assert!(
                    admitted,
                    "{} is {child_side:?} but its container {container} is {container_side:?}",
                    capability.as_str()
                );
            }
        }
    }

    #[test]
    fn the_table_covers_every_capability_exactly_once() {
        for capability in Capability::ALL {
            let count = CAPABILITY_TABLE
                .iter()
                .filter(|(candidate, _)| candidate.as_str() == capability.as_str())
                .count();
            assert_eq!(count, 1, "{} appears {count} times", capability.as_str());
        }
        assert_eq!(CAPABILITY_TABLE.len(), Capability::ALL.len());
    }

    #[test]
    fn every_table_row_names_a_known_revision() {
        for (capability, versions) in CAPABILITY_TABLE {
            assert!(
                !versions.is_empty(),
                "{} has no versions",
                capability.as_str()
            );
            for version in *versions {
                assert!(
                    version.is_known(),
                    "{} cites {version}",
                    capability.as_str()
                );
            }
            // Sorted, so the list reads as a history rather than a set.
            assert!(
                versions.is_sorted(),
                "{} versions are unsorted",
                capability.as_str()
            );
        }
    }

    /// The load-bearing test. It reads the real vendored schemas and checks
    /// this module's table against them, in both directions, for all six
    /// revisions — so a table row that is wrong, or a capability that upstream
    /// added, fails here rather than producing a nonsensical feature profile
    /// later.
    #[test]
    fn the_table_matches_every_vendored_schema() {
        let spec_root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../protocol-droid-schema/spec");

        let mut mismatches: Vec<String> = Vec::new();
        let mut revisions_checked = 0usize;

        for version in ProtocolVersion::KNOWN {
            let path = spec_root.join(version.to_string()).join("schema.json");
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(document) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            revisions_checked += 1;

            let defs = document
                .get("$defs")
                .or_else(|| document.get("definitions"))
                .and_then(serde_json::Value::as_object);

            for (root, side) in [
                ("ClientCapabilities", Side::Client),
                ("ServerCapabilities", Side::Server),
            ] {
                let found = leaf_names(defs, root);
                for name in &found {
                    let modelled = Capability::ALL
                        .iter()
                        .find(|c| c.as_str() == name.as_str())
                        .copied();
                    match modelled {
                        None => mismatches.push(format!(
                            "{version} {root}.{name}: in the schema, not modelled"
                        )),
                        Some(capability) => {
                            if !capability.side().includes_client() && side == Side::Client
                                || !capability.side().includes_server() && side == Side::Server
                            {
                                mismatches.push(format!(
                                    "{version} {root}.{name}: declared on the wrong side"
                                ));
                            }
                            if !defined_in(capability, version) {
                                mismatches.push(format!(
                                    "{version} {root}.{name}: defined in the schema, \
                                     absent from CAPABILITY_TABLE"
                                ));
                            }
                        }
                    }
                }
                // The other direction: a table row that the schema no longer has.
                for (capability, versions) in CAPABILITY_TABLE {
                    if !versions.contains(&version) {
                        continue;
                    }
                    let declared_here = capability.side().includes_client() && side == Side::Client
                        || capability.side().includes_server() && side == Side::Server;
                    if declared_here && !found.contains(capability.as_str()) {
                        mismatches.push(format!(
                            "{version} {root}.{}: in CAPABILITY_TABLE, not in the schema",
                            capability.as_str()
                        ));
                    }
                }
            }
        }

        assert!(
            revisions_checked >= 5,
            "only checked {revisions_checked} schemas"
        );
        assert!(
            mismatches.is_empty(),
            "CAPABILITY_TABLE disagrees with the vendored schemas:\n  {}",
            mismatches.join("\n  ")
        );
    }

    /// Collects every dotted name under `root`, e.g. `tasks.requests.tools.call`.
    ///
    /// Recurses all the way down rather than a fixed depth: `2025-11-25` nests
    /// four levels under `tasks`, and a walker that stopped at one would report
    /// six table rows as missing from the schema.
    fn leaf_names(
        defs: Option<&serde_json::Map<String, serde_json::Value>>,
        root: &str,
    ) -> BTreeSet<String> {
        fn walk(node: &serde_json::Value, prefix: &str, out: &mut BTreeSet<String>) {
            for (name, value) in node
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .into_iter()
                .flatten()
            {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                out.insert(path.clone());
                walk(value, &path, out);
            }
        }

        let mut out = BTreeSet::new();
        let Some(node) = defs.and_then(|d| d.get(root)) else {
            return out;
        };
        walk(node, "", &mut out);
        out
    }

    #[test]
    fn sets_behave_like_sets() {
        let empty = CapabilitySet::empty();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert!(!empty.contains(Capability::Tools));

        let one = empty.with(Capability::Tools);
        assert!(one.contains(Capability::Tools));
        assert!(!one.is_empty());
        assert_eq!(one.len(), 1);

        // Idempotent.
        assert_eq!(one.with(Capability::Tools), one);
        assert_eq!(one.with(Capability::Roots).len(), 2);
    }

    #[test]
    fn all_for_is_scoped_to_one_revision() {
        let oldest = CapabilitySet::all_for(ProtocolVersion::V2024_11_05);
        assert!(oldest.contains(Capability::Roots));
        assert!(oldest.contains(Capability::Tools));
        assert!(oldest.contains(Capability::Logging));
        // Nothing that arrived later.
        assert!(!oldest.contains(Capability::Elicitation));
        assert!(!oldest.contains(Capability::Completions));
        assert!(!oldest.contains(Capability::Tasks));
        assert!(!oldest.contains(Capability::Extensions));

        let newest = CapabilitySet::all_for(ProtocolVersion::V2026_07_28);
        assert!(newest.contains(Capability::Elicitation));
        assert!(newest.contains(Capability::Extensions));
        // The two that came and went.
        assert!(!newest.contains(Capability::Tasks));
        assert!(!newest.contains(Capability::RootsListChanged));
        // Still there despite the methods behind it changing.
        assert!(newest.contains(Capability::ResourcesSubscribe));
    }

    #[test]
    fn restricting_drops_what_a_revision_cannot_express() {
        // A newer peer advertises tasks to an older peer.
        let newer = CapabilitySet::all_for(ProtocolVersion::V2025_11_25);
        assert!(newer.contains(Capability::Tasks));
        let downgraded = newer.restricted_to(ProtocolVersion::V2024_11_05);
        assert!(!downgraded.contains(Capability::Tasks));
        assert!(!downgraded.contains(Capability::Elicitation));
        assert!(downgraded.contains(Capability::Tools));
        assert!(downgraded.contains(Capability::RootsListChanged));
    }

    #[test]
    fn an_unknown_revision_defines_nothing() {
        let future = ProtocolVersion::Unknown("2027-01-01".parse().unwrap());
        for capability in Capability::ALL {
            assert!(!defined_in(capability, future), "{}", capability.as_str());
        }
        // So restricting a real set to it leaves nothing at all.
        let set = CapabilitySet::all();
        assert!(set.restricted_to(future).is_empty());
    }

    #[test]
    fn iteration_is_declaration_ordered_and_complete() {
        let set = CapabilitySet::all().with(Capability::Experimental);
        let collected: Vec<Capability> = set.iter().collect();
        assert_eq!(collected, Capability::ALL.to_vec());
        // Duplicates do not double up.
        let doubled = set.with(Capability::Tools).with(Capability::Tools);
        assert_eq!(doubled.len() as usize, Capability::ALL.len());
    }

    #[test]
    fn tasks_is_defined_in_exactly_one_revision() {
        for version in ProtocolVersion::KNOWN {
            let expected = version == ProtocolVersion::V2025_11_25;
            assert_eq!(
                defined_in(Capability::Tasks, version),
                expected,
                "tasks in {version}"
            );
            for nested in [
                Capability::TasksCancel,
                Capability::TasksList,
                Capability::TasksRequests,
                Capability::TasksRequestsTools,
                Capability::TasksRequestsToolsCall,
                Capability::TasksRequestsSampling,
                Capability::TasksRequestsSamplingCreateMessage,
                Capability::TasksRequestsElicitation,
                Capability::TasksRequestsElicitationCreate,
            ] {
                assert_eq!(
                    defined_in(nested, version),
                    expected,
                    "{nested:?} in {version}"
                );
            }
        }
    }

    #[test]
    fn sides_are_as_the_schemas_declare_them() {
        assert_eq!(Capability::Roots.side(), Side::Client);
        assert_eq!(Capability::SamplingTools.side(), Side::Client);
        assert_eq!(Capability::Tools.side(), Side::Server);
        assert_eq!(Capability::TasksRequestsTools.side(), Side::Server);
        assert_eq!(Capability::Experimental.side(), Side::Both);
        assert_eq!(Capability::Extensions.side(), Side::Both);
        assert_eq!(Capability::Tasks.side(), Side::Both);
    }
}
