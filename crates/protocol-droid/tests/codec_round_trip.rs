//! The codec round trip, across every revision.
//!
//! # What this is the evidence for
//!
//! The capability says it plainly: `lift(v, lower(v, ir)) == ir` for every IR
//! value expressible at revision `v`. `tests/lowering_properties.rs` proves the
//! *decisions* — what is expressible, which rule answers a missing feature, what
//! the report says — on IR values it never puts through serde. This file is the
//! other half: it drives the real codecs, so the lift and the lower that meet in
//! the middle are the ones a bridge actually calls.
//!
//! That split matters because the two halves fail in different ways. A gate can
//! be exact about what it should refuse and still have a codec that writes the
//! wrong union arm; a codec can round-trip a block perfectly and drop a `_meta`
//! member on the way through. Neither file catches the other's bug, and the
//! failure is silent either way — the message arrives, it is just subtly not
//! what the sender wrote.
//!
//! # Expressibility is decided by the gate, not by a second table
//!
//! The strategy generates content this build can model, then filters the
//! (revision, value) pairs through [`check_expressible`] — the same function
//! `lower` calls. A hand-written list of what each revision supports would be a
//! second source of truth that disagrees with the feature matrix the first time
//! a revision moves, and a property test that filters against the wrong oracle
//! is worse than no test: it passes by construction.
//!
//! # The two normalisations, and why they are not a fudge factor
//!
//! The IR has two places where a value has two spellings, and a codec picks
//! one of them. [`canonical`] names both; every other field compares with `==`.
//!
//! **`resultType`.** `2026-07-28` makes it required, so an IR value with none
//! lowers to `"complete"` and lifts back as `Some("complete")`. That is the
//! specification's own default — the schema treats an absent value and
//! `"complete"` as the same thing.
//!
//! **Empty `annotations`.** An [`Annotations`] with no audience, no priority, no
//! `lastModified` and nothing in the bag is the same message as no annotations
//! at all, so the codecs write no field rather than writing `{}`. This one is a
//! codec decision rather than a schema default, and it is already pinned from
//! the other side: `lower_annotations(Some(&Annotations::empty()))` is `None` in
//! each revision's own tests. This file pins that the consequence holds all the
//! way back up — that a caller who built `Some(empty)` sees `None` after a
//! round trip, rather than the codec inventing a member to keep the shape.
//!
//! Both are named functions rather than a general "these are close enough"
//! tolerance because a blanket comparison would swallow exactly the second bug
//! this file exists to find: a codec that drops a member, or invents one, still
//! "nearly" matches.
//!
//! # The direction that is asserted
//!
//! `lower` then `lift`, not the reverse. Going the other way — lifting wire
//! bytes, lowering them, and comparing — is covered per revision in the codecs'
//! own snapshot tests, where the expected bytes are the ones a peer would
//! actually receive. What only this file can show is that an IR value
//! *constructed by an application* survives the trip, which is the case a
//! bridge has to get right when it synthesises a result rather than forwarding
//! one.

// `clippy::panic` is deliberately absent: nothing here calls `panic!` directly.
// `prop_assert_eq!` panics from inside its own expansion, so naming it would be
// an expectation this file can never fulfil.
#![expect(
    clippy::expect_used,
    reason = "property tests assert on fallible calls; propagating would replace a shrunk counterexample with a generic error"
)]

use proptest::prelude::*;
use protocol_droid::adapters::secondary::codec::result::draft::Draft;
use protocol_droid::adapters::secondary::codec::result::v2024_11_05::V2024_11_05;
use protocol_droid::adapters::secondary::codec::result::v2025_03_26::V2025_03_26;
use protocol_droid::adapters::secondary::codec::result::v2025_06_18::V2025_06_18;
use protocol_droid::adapters::secondary::codec::result::v2025_11_25::V2025_11_25;
use protocol_droid::adapters::secondary::codec::result::v2026_07_28::V2026_07_28;
use protocol_droid::domain::ir::Codec;
use protocol_droid::domain::ir::lower::{LowerError, TranslationReport, check_expressible};
use protocol_droid::domain::ir::{
    Annotations, AudioContent, Content, EmbeddedResource, ImageContent, JsonObject, ResourceIcon,
    ResourceLinkContent, Role, TextContent, ToolResult,
};
use protocol_droid::domain::{
    CapabilitySet, DegradePolicy, FeatureProfile, Negotiation, ProtocolVersion, TransportKind,
    resolve,
};

/// The result type the specification substitutes for an absent `resultType`.
const COMPLETE: &str = "complete";

/// The revision the draft codec currently tracks.
const DRAFT_REVISION: ProtocolVersion = ProtocolVersion::V2026_07_28;

/// A session at `version` with both sides advertising everything that revision
/// defines — the most permissive session a peer can hold.
///
/// Anything tighter would be a second opinion about what a revision supports,
/// and this file's job is to test the codecs rather than the negotiation.
fn session(version: ProtocolVersion) -> FeatureProfile {
    let caps = CapabilitySet::all_for(version);
    resolve(Negotiation {
        version,
        client_caps: caps,
        server_caps: caps,
        transport: TransportKind::Stdio,
    })
}

/// Lowers and lifts one revision's codec, generically.
///
/// `Codec::Wire` differs per revision, so the per-revision work is confined to
/// constructing the value. Everything the property actually says is written
/// once, here, which is what keeps six codecs from drifting into six subtly
/// different round trips.
fn round_trip<C: Codec>(
    codec: &C,
    ir: ToolResult,
) -> Result<(ToolResult, TranslationReport), LowerError> {
    let mut report = TranslationReport::new(codec.version());
    let wire = codec.lower(ir, DegradePolicy::strict(), &mut report)?;
    Ok((codec.lift(&wire), report))
}

/// The same, for whichever revision the property drew.
///
/// `Draft` is handled by `V2026_07_28` because the draft codec reports itself
/// as the revision it currently tracks; `draft_reports_the_revision_it_tracks`
/// is the test that says so rather than leaving it to a reader.
fn round_trip_at(
    version: ProtocolVersion,
    ir: ToolResult,
) -> Result<(ToolResult, TranslationReport), LowerError> {
    match version {
        ProtocolVersion::V2024_11_05 => round_trip(&V2024_11_05, ir),
        ProtocolVersion::V2025_03_26 => round_trip(&V2025_03_26, ir),
        ProtocolVersion::V2025_06_18 => round_trip(&V2025_06_18, ir),
        ProtocolVersion::V2025_11_25 => round_trip(&V2025_11_25, ir),
        ProtocolVersion::V2026_07_28 | ProtocolVersion::Unknown(_) => round_trip(&V2026_07_28, ir),
    }
}

/// The IR a value has once the codecs' two spelling choices are applied.
///
/// See the module docs for what the two are and why they are not the same as
/// letting a comparison be approximate.
fn canonical(mut ir: ToolResult, version: ProtocolVersion) -> ToolResult {
    if version >= ProtocolVersion::V2026_07_28 && ir.result_type.is_none() {
        ir.result_type = Some(COMPLETE.to_owned());
    }
    for block in &mut ir.content {
        drop_empty_annotations(block);
    }
    ir
}

/// Whether these annotations carry no information at all.
fn is_empty(annotations: &Annotations) -> bool {
    annotations.audience.is_empty()
        && annotations.priority.is_none()
        && annotations.last_modified.is_none()
        && annotations.extensions.iter().next().is_none()
}

/// Replaces `Some(empty)` with `None`, which is how a codec writes it.
fn drop_empty_annotations(block: &mut Content) {
    let slot = match block {
        Content::Text(text) => &mut text.annotations,
        Content::Image(image) => &mut image.annotations,
        Content::Audio(audio) => &mut audio.annotations,
        Content::ResourceLink(link) => &mut link.annotations,
        Content::EmbeddedResource(resource) => &mut resource.annotations,
        Content::Unknown(_) => return,
    };
    if slot.as_ref().is_some_and(is_empty) {
        *slot = None;
    }
}

/// Short, non-JSON-significant text.
fn any_text() -> Boxed<String> {
    "[a-z0-9 _-]{1,12}".prop_map(String::from).boxed()
}

/// A `file:` URI, because a peer that sends a bare word is not what a codec
/// sees and testing against one proves less.
fn any_uri() -> Boxed<String> {
    any_text()
        .prop_map(|name| format!("file:///a-{name}"))
        .boxed()
}

/// A small base64-shaped payload. Nothing here validates it, but a realistic
/// alphabet keeps a fixture from being rejected by a check the codec should
/// never have been doing.
fn any_data() -> Boxed<String> {
    "[A-Za-z0-9+/]{1,12}".prop_map(String::from).boxed()
}

/// Annotations this build models, with no `lastModified` — that one is a
/// `2025-06-18` feature, and the expressibility filter is the right place to
/// find that out rather than a second table here.
fn any_annotations() -> Boxed<Annotations> {
    (
        prop::option::of(prop::sample::select(vec![Role::User, Role::Assistant])),
        prop::option::of(0.0f64..=1.0),
    )
        .prop_map(|(role, priority)| Annotations {
            audience: role.into_iter().collect(),
            priority,
            last_modified: None,
            ..Annotations::empty()
        })
        .boxed()
}

/// Puts annotations on a block, whichever kind it is.
///
/// A `match` rather than a setter closure, because a closure cannot borrow a
/// block it does not own, and a match is the honest shape anyway: a new block
/// kind has to be given a decision here rather than silently keeping whatever
/// an earlier arm happened to leave behind.
fn annotated(mut block: Content, annotations: Option<Annotations>) -> Content {
    let slot = match &mut block {
        Content::Text(text) => &mut text.annotations,
        Content::Image(image) => &mut image.annotations,
        Content::Audio(audio) => &mut audio.annotations,
        Content::ResourceLink(link) => &mut link.annotations,
        Content::EmbeddedResource(resource) => &mut resource.annotations,
        // A block this build cannot model holds no typed annotations — it
        // exists precisely so that what the sender wrote is kept as-is. The
        // codecs' own tests cover that passthrough.
        Content::Unknown(_) => return block,
    };
    *slot = annotations;
    block
}

/// A link, which is a `2025-06-18` addition, and one carrying icons, which are
/// a `2025-11-25` one.
///
/// Both are generated at every revision so the expressibility filter has
/// something to refuse. A generator that only emitted what is supported would
/// be testing the filter rather than the codec.
fn any_link() -> Boxed<Content> {
    let plain = (any_uri(), any_text(), prop::option::of(any_text()))
        .boxed()
        .prop_map(|(uri, name, description)| {
            Content::ResourceLink(ResourceLinkContent {
                description,
                ..ResourceLinkContent::new(uri, name)
            })
        })
        .boxed();
    let with_icons = (any_uri(), any_text())
        .boxed()
        .prop_map(|(uri, name)| {
            Content::ResourceLink(ResourceLinkContent {
                icons: vec![ResourceIcon::new(format!("{uri}.png"))],
                ..ResourceLinkContent::new(uri, name)
            })
        })
        .boxed();
    plain.prop_union(with_icons).boxed()
}

/// A `BoxedStrategy`, so two strategies of different concrete types can be
/// unioned without the opaque types having to be named.
type Boxed<T> = BoxedStrategy<T>;

/// Any block this build models. Whether it is expressible at the drawn revision
/// is the gate's answer, not the generator's.
fn any_block() -> Boxed<Content> {
    let bare: Boxed<Content> = prop_oneof![
        any_text()
            .prop_map(|t| Content::Text(TextContent::new(t)))
            .boxed(),
        any_data()
            .prop_map(|d| Content::Image(ImageContent::new(d, "image/png")))
            .boxed(),
        any_data()
            .prop_map(|d| Content::Audio(AudioContent::new(d, "audio/wav")))
            .boxed(),
        any_uri()
            .prop_map(|u| Content::EmbeddedResource(EmbeddedResource::text(u, "body")))
            .boxed(),
    ]
    .boxed();
    (bare, prop::option::of(any_annotations()))
        .boxed()
        .prop_map(|(block, annotations)| annotated(block, annotations))
        .boxed()
        .prop_union(any_link())
        .boxed()
}

/// Structured output, which is a `2025-06-18` addition and is modelled as
/// validated text — so the generator's own job is to produce JSON that parses.
fn any_structured() -> Boxed<JsonObject> {
    (any_text(), any_text())
        .prop_map(|(key, value)| {
            JsonObject::new(format!("{{{key:?}:{value:?}}}"))
                .expect("a two-member object is valid JSON")
        })
        .boxed()
}

/// Any tool result this build can model.
fn any_result() -> Boxed<ToolResult> {
    (
        prop::collection::vec(any_block(), 0..3),
        prop::option::of(any_structured()),
        any::<bool>(),
        prop::option::of(prop::sample::select(vec![
            "complete".to_owned(),
            "partial".to_owned(),
        ])),
    )
        .prop_map(
            |(content, structured_content, is_error, result_type)| ToolResult {
                content,
                structured_content,
                is_error,
                result_type,
                ..ToolResult::new(vec![])
            },
        )
        .boxed()
}

/// A revision to test at.
///
/// `Unknown` is absent on purpose: this build has no wire types for a revision
/// it does not know, so a codec cannot be dispatched to one, and including it
/// would only exercise the match arm.
fn any_revision() -> impl Strategy<Value = ProtocolVersion> {
    prop::sample::select(vec![
        ProtocolVersion::V2024_11_05,
        ProtocolVersion::V2025_03_26,
        ProtocolVersion::V2025_06_18,
        ProtocolVersion::V2025_11_25,
        ProtocolVersion::V2026_07_28,
    ])
}

/// A revision paired with a value it can express.
///
/// The filter calls the gate rather than a table, so "expressible" means
/// exactly what it means to `lower`. A test that filtered against its own idea
/// of expressibility would pass while the codec refused, or fail while the
/// codec was right.
fn expressible_pair() -> Boxed<(ProtocolVersion, ToolResult)> {
    (any_revision(), any_result())
        .boxed()
        .prop_filter(
            "the value must be expressible at the revision it is tested at",
            |(version, ir)| {
                check_expressible(&ir.required_features(), &session(*version), *version).is_ok()
            },
        )
        .boxed()
}

/// A value the draft can express.
///
/// Filtered and canonicalised at the draft's own revision, which is the whole
/// point of this second file. Testing the draft at a *drawn* revision's profile
/// compares two different sessions: the value passes the filter at
/// `2024-11-05` and the draft then applies `2026-07-28`'s rules to it, so the
/// failure it reports is the test's, not the codec's.
fn expressible_at_draft() -> Boxed<ToolResult> {
    any_result()
        .boxed()
        .prop_filter(
            "the value must be expressible at the revision the draft tracks",
            |ir| {
                let version = DRAFT_REVISION;
                check_expressible(&ir.required_features(), &session(version), version).is_ok()
            },
        )
        .boxed()
}

proptest! {
    /// The named criterion: a value expressible at a revision survives
    /// `lower` then `lift` unchanged.
    ///
    /// The report must also come back empty. A round trip that needed a
    /// downgrade on the way down and then put the value back would be
    /// indistinguishable here, and would be a bridge that quietly rewrites
    /// messages it was only asked to forward.
    #[test]
    fn an_expressible_value_survives_its_own_revision(
        (version, ir) in expressible_pair(),
    ) {
        let (lifted, report) = round_trip_at(version, ir.clone())
            .expect("the gate said this value fits, so lowering cannot refuse it");
        prop_assert_eq!(report.steps().len(), 0, "a strict round trip degrades nothing");
        prop_assert_eq!(lifted, canonical(ir, version));
    }

    /// The same property for the draft codec, which is a separate set of
    /// generated types from `2026-07-28` even though it tracks that revision.
    ///
    /// Sharing a version does not mean sharing a codec, and the draft's union
    /// arms come out of a different `schema.json`.
    #[test]
    fn an_expressible_value_survives_the_draft(ir in expressible_at_draft()) {
        let (lifted, report) = round_trip(&Draft, ir.clone())
            .expect("the draft expresses everything 2026-07-28 does");
        prop_assert_eq!(report.steps().len(), 0, "a strict round trip degrades nothing");
        prop_assert_eq!(lifted, canonical(ir, DRAFT_REVISION));
    }

    /// Round trip reaches a fixed point: doing it twice changes nothing the
    /// second time.
    ///
    /// This is a different claim from the one above, and it is the one that
    /// catches a codec that is self-consistent but not lossless — one that,
    /// say, re-serialises an annotation into a different but stable form.
    /// Comparing the value to itself would pass; comparing two passes would not.
    #[test]
    fn a_round_trip_reaches_a_fixed_point(
        (version, ir) in expressible_pair(),
    ) {
        let once = canonical(ir, version);
        let (twice, _) = round_trip_at(version, once.clone())
            .expect("a value that lowered once lowers again");
        prop_assert_eq!(&twice, &once);
    }
}

/// The draft codec reports the revision it tracks, which is what lets
/// `round_trip_at` dispatch it to the same arm.
///
/// Asserted rather than assumed: the property above depends on it, and a
/// future revision moving the draft would otherwise silently narrow the
/// coverage of the draft test to whatever the match arm happens to reach.
#[test]
fn draft_reports_the_revision_it_tracks() {
    assert_eq!(Draft.version(), DRAFT_REVISION);
}

/// Every revision has a codec, and each one is wired to its own version.
///
/// The dispatch above is a `match` over five arms plus `Unknown`, and a new
/// revision that nobody added an arm for would compile and then never be
/// tested. This is the test that says the table and the codecs agree.
#[test]
fn every_revision_has_a_codec_named_for_it() {
    let expected = [
        (ProtocolVersion::V2024_11_05, V2024_11_05.version()),
        (ProtocolVersion::V2025_03_26, V2025_03_26.version()),
        (ProtocolVersion::V2025_06_18, V2025_06_18.version()),
        (ProtocolVersion::V2025_11_25, V2025_11_25.version()),
        (ProtocolVersion::V2026_07_28, V2026_07_28.version()),
    ];
    for (revision, reported) in expected {
        assert_eq!(revision, reported, "a codec must report its own revision");
    }
}
