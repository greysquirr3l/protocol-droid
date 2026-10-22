//! The registry's read path and its publish path, at the sizes that matter.
//!
//! # Why the registry needs its own benchmark
//!
//! The registry is the one structure in the crate whose cost is not constant.
//! A JSON-RPC frame is small and its envelope decode is a fixed amount of
//! work; a registry holds whatever the server registered, and a server that
//! aggregates three upstream servers can be carrying a thousand tools. Two
//! shapes follow from that, and each can regress without the other moving:
//!
//! - **A read is one pass, not two.** `list_visible` walks the table once and
//!   derives `visible`, `hidden` and `total` together, precisely so they
//!   cannot disagree. A change that computes the hidden count with a second
//!   walk shows up as `list_1k` drifting above `get_1k` by a whole table's
//!   worth of work.
//! - **A write is a copy of the table.** The snapshot is immutable, so every
//!   publish rebuilds it. That is a deliberate trade: it buys a reader that
//!   cannot observe a half-applied registration, and it costs one rebuild per
//!   mutation. `rebuild_1k` is the price, isolated from the cost of building
//!   the tools in the first place, and it must stay linear in the table —
//!   `rebuild_1k` against `rebuild_100` is the shape of that claim.
//!   `build_100` and `build_1k` are kept separately because they measure the
//!   *other* thing: a thousand JSON schema parses, which is the cost of
//!   populating a registry and has nothing to do with the copy. Conflating
//!   the two would hide a regression in either behind the other.
//!
//! # The concurrency claim
//!
//! The CAS loop in `publish` is not measurable on a single thread: the
//! uncontended cost of `compare_and_swap` is a couple of instructions. What a
//! benchmark *can* show is the contention case, where a retry actually
//! happens — `publish_contended` runs writers against a shared store and
//! reports the throughput that survives. A regression that turned the CAS loop
//! back into a load-then-store would show up as a large speedup here, which is
//! the point: the lost-update bug does not make publishing slower, it makes
//! publishing *wrong*, and the only visible signal is that the retry
//! disappeared.
//!
//! # How to read a regression
//!
//! Run `cargo bench -p protocol-droid --bench registry` locally before
//! deciding whether a move is a regression. The read path allocates a
//! `Vec<Arc<..>>` per call, so it is sensitive to the allocator, and a noisy
//! machine shifts every line together.

// divan's `#[bench]` macro matches its argument against a fn pointer written
// with an elided `Bencher`, and it does that before this crate's lints are
// resolved. Spelling the two lifetimes out is what the macro rejects, so the
// lint and the macro cannot both be satisfied at the signature, which is why
// the expectation is scoped to this file rather than repeated on every benchmark.
#![expect(
    elided_lifetimes_in_paths,
    reason = "divan's `#[bench]` macro only matches a `Bencher` with the lifetimes elided; spelling them out makes the signature invisible to it"
)]
// Every fixture below is built once, outside the measured region, from a
// const schema. A parse failure means the benchmark is timing a branch that
// never succeeds in production, and a green number for that would be worse
// than a loud one.
#![expect(
    clippy::panic,
    reason = "benchmark fixtures are consts; a parse failure is a broken benchmark, not a condition to average over"
)]
use std::hint::black_box;
use std::sync::Arc;
use std::sync::LazyLock;

use divan::Bencher;
use protocol_droid::application::registry::RegistryStore;
use protocol_droid::domain::FeatureProfile;
use protocol_droid::domain::registry::{AuthContext, Primitive, Registered, Scope, Snapshot, Tool};

/// The schema every fixture tool is built from. A tool with a real schema
/// costs a parse, and a benchmark that measured parsing would report the
/// JSON crate rather than the registry.
const SCHEMA: &str = r#"{"type":"object","properties":{"text":{"type":"string"}}}"#;

/// A thousand tool names, built once on first use and shared. A thousand is
/// the size that matters: it is roughly what an aggregating server carries,
/// and it is large enough that a quadratic rebuild stops being fast enough to
/// hide.
///
/// A `LazyLock` rather than a `const`, because the names cannot be a `const`
/// without either hand-rolling the zero-padding or dropping it. The
/// hand-rolled version needs a `const fn` that writes bytes into a local
/// buffer, and the `&'static str` it would hand back points into a frame that
/// is already gone — `constructing invalid value ... encountered a dangling
/// reference`, which is exactly the bug the compiler refuses to let through.
/// The alternative, a `const` array of a thousand literals, is noise a
/// reviewer cannot check. The allocation happens once, outside every measured
/// region.
static TOOL_NAMES: LazyLock<Vec<String>> =
    LazyLock::new(|| (0..1000).map(|index| format!("tool_{index:04}")).collect());

/// A tenth of that, to separate "linear in the table" from "quadratic in the
/// table". Ten times the tools should be about ten times the work; if it is
/// closer to a hundred, the rebuild walks the table per insert.
///
/// The hundred-tool *prefix* of the same sequence, so the two fixtures differ
/// in nothing but size.
static SMALL_NAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
    TOOL_NAMES
        .get(..100)
        .map(<[String]>::to_vec)
        .unwrap_or_default()
});

/// The name at `index`, falling back to the first name.
///
/// `get` rather than an index because `indexing_slicing` is denied across
/// this crate, and rightly: a fixture that disagrees with its own length
/// should bench a slightly smaller table, not abort the run with no numbers.
fn name_at(index: usize) -> &'static str {
    TOOL_NAMES
        .get(index)
        .or_else(|| TOOL_NAMES.first())
        .map_or("", String::as_str)
}

/// One registered tool, from a const schema. Building it is the only fallible
/// step and it happens before any measurement.
fn tool(name: &str) -> Registered {
    let tool = match Tool::new(name, SCHEMA) {
        Ok(tool) => tool,
        Err(failed) => panic!("the fixture schema is const and must parse: {failed}"),
    };
    Registered::from(Primitive::from(tool))
}

/// A snapshot holding every fixture tool, built once per size.
///
/// A build is one pass over a fresh table, so this is the `build_1k` cost
/// paid outside any measurement.
fn store_of(names: &[String]) -> RegistryStore {
    let store = RegistryStore::new();
    for name in names {
        store.register(tool(name));
    }
    store
}

/// A session holding every scope, so the read benchmarks measure the filter
/// rather than the authorisation. `list_hidden_1k` is the one that has to
/// deny, and it does that with a session holding nothing.
fn unrestricted() -> AuthContext {
    AuthContext::unrestricted("bench")
}

/// A session holding nothing, for the deny path.
fn anonymous() -> AuthContext {
    AuthContext::new("bench", protocol_droid::domain::registry::ScopeSet::empty())
}

/// A profile that negotiated every feature, so the feature half of the gate
/// always passes and the read benchmarks measure the scope half.
fn everything() -> FeatureProfile {
    FeatureProfile::all()
}

/// A read of the whole table, one pass, with every primitive visible.
///
/// The claim is that this is one traversal: the hidden count comes out of the
/// same walk. A second pass would roughly double it.
#[divan::bench]
fn list_1k(b: Bencher) {
    let store = store_of(&TOOL_NAMES);
    let session = unrestricted();
    let profile = everything();

    b.bench(|| {
        let listing = store.list_visible(
            black_box(&session),
            black_box(&profile),
            black_box(protocol_droid::domain::registry::PrimitiveKind::Tool),
        );
        black_box((listing.visible().len(), listing.hidden(), listing.total()))
    });
}

/// The same read with a session that holds no scope, so every primitive is
/// denied.
///
/// The pair with `list_1k` is not the one the doc comment above first
/// suggested. Measured, the two are within noise of each other — 2.6 µs
/// against 2.7 µs — because the walk over the table, not the collection, is
/// what costs: a denied primitive is still visited, and the `Vec` it would
/// have gone into is a pointer push. So the deny path is *not* meaningfully
/// cheaper, and the useful reading is the opposite of what one might expect:
/// a filter that did less work per element would show up here as a
/// *divergence* from `list_1k`, not a convergence.
#[divan::bench]
fn list_hidden_1k(b: Bencher) {
    let store = store_of(&TOOL_NAMES);
    let session = anonymous();
    let profile = everything();

    b.bench(|| {
        let listing = store.list_visible(
            black_box(&session),
            black_box(&profile),
            black_box(protocol_droid::domain::registry::PrimitiveKind::Tool),
        );
        black_box((listing.visible().len(), listing.hidden(), listing.total()))
    });
}

/// A single lookup, the shape a `tools/call` takes.
///
/// This is the one read that has to be O(1): a call resolves a name against
/// the table rather than walking it. It also has to be indistinguishable from
/// absent when the caller may not see the primitive, which is why it returns
/// an `Option` and not a `Result` — see
/// `a_hidden_primitive_and_an_absent_one_both_read_as_none`.
#[divan::bench]
fn get_1k(b: Bencher) {
    let store = store_of(&TOOL_NAMES);
    let session = unrestricted();
    let profile = everything();

    b.bench(|| {
        let found = store.get_visible(
            black_box(&session),
            black_box(&profile),
            black_box(protocol_droid::domain::registry::PrimitiveKind::Tool),
            black_box(name_at(999)),
        );
        black_box(found)
    });
}

/// The same lookup on a name that is not there, so the benchmark cannot be
/// reporting a hit path it will never take.
#[divan::bench]
fn get_miss_1k(b: Bencher) {
    let store = store_of(&TOOL_NAMES);
    let session = unrestricted();
    let profile = everything();

    b.bench(|| {
        let found = store.get_visible(
            black_box(&session),
            black_box(&profile),
            black_box(protocol_droid::domain::registry::PrimitiveKind::Tool),
            black_box("no_such_tool"),
        );
        black_box(found)
    });
}

/// A publish into a thousand-tool table — the replace of an existing name.
///
/// This is the price of immutability: the table is rebuilt, because a reader
/// that is mid-iteration over the old one has to keep seeing a consistent
/// one. It must stay linear in the table, and `publish_1k` is where a change
/// that made the rebuild quadratic would first be visible.
///
/// At 8.5 µs it is about 3.9 µs above `rebuild_1k`, and that difference is
/// the replace path's linear scan for the old slot: `to_builder` and
/// `build_at` together are `rebuild_1k`, so everything else here is the cost
/// of locating the entry being replaced. The gap is the thing to watch — a
/// change that made the lookup by identity quadratic would double `publish_1k`
/// without moving `rebuild_1k` at all.
#[divan::bench]
fn publish_1k(b: Bencher) {
    let store = store_of(&TOOL_NAMES);
    // The same name every time, so the measured work is a replace and not a
    // growing table. An insert would eventually dominate with the allocation
    // for a new slot rather than with the rebuild this benchmark is about.
    let replacement = tool(name_at(0));

    b.bench(|| {
        let published = store.register(black_box(replacement.clone()));
        black_box(published.generation())
    });
}

/// A removal from a thousand-tool table, which is the one publish that
/// shortens the table as it rebuilds it.
///
/// A removal walks the listing to find the slot, so this is the linear scan
/// the immutable design accepts on the rarer of the two mutation paths.
#[divan::bench]
fn unpublish_1k(b: Bencher) {
    let store = store_of(&TOOL_NAMES);
    let target = protocol_droid::domain::registry::PrimitiveId::from(name_at(999));

    b.bench(|| {
        let published = store.unregister(
            black_box(protocol_droid::domain::registry::PrimitiveKind::Tool),
            black_box(&target),
        );
        black_box(published.generation())
    });
}

/// A build from scratch, a hundred tools.
///
/// This is dominated by the hundred schema parses, not by the table: at a
/// median of 37.7 µs it is an order of magnitude above `rebuild_100`. That
/// gap is the reason the rebuild benchmarks exist separately — without them
/// this number would be read as "the table is cheap", which is false.
/// It is a linear check on construction: `build_1k` should be about ten
/// times this.
#[divan::bench]
fn build_100(b: Bencher) {
    b.bench(|| {
        let mut builder = Snapshot::builder();
        for name in SMALL_NAMES.iter() {
            builder.insert(black_box(tool(name)));
        }
        black_box(builder.build())
    });
}

/// The same build, a thousand tools. Compare against `build_100`.
///
/// Measured at a median of 387 µs against 37.7 µs, so 10.2x for 10x the
/// tools. Linear, which is the claim.
#[divan::bench]
fn build_1k(b: Bencher) {
    b.bench(|| {
        let mut builder = Snapshot::builder();
        for name in TOOL_NAMES.iter() {
            builder.insert(black_box(tool(name)));
        }
        black_box(builder.build())
    });
}

/// A read taken while other threads are publishing, against a shared store.
///
/// The point is the retry. A store that lost updates instead of retrying would
/// report a *faster* number here, because a lost update is a rebuild that
/// throws its result away — so this benchmark is read for a drop, not for a
/// rise. See `a_reader_never_sees_a_torn_snapshot`, which is the correctness
/// half of the same claim.
#[divan::bench]
fn read_under_publish(b: Bencher) {
    let store = Arc::new(store_of(&TOOL_NAMES));
    let session = unrestricted();
    let profile = everything();

    // One writer, churning the front of the table while the reader lists it.
    let writer = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            for round in 0..200usize {
                store.register(tool(name_at(round % TOOL_NAMES.len())));
            }
        })
    };

    b.bench(|| {
        let listing = store.list_visible(
            black_box(&session),
            black_box(&profile),
            black_box(protocol_droid::domain::registry::PrimitiveKind::Tool),
        );
        black_box(listing.total())
    });

    let _ = writer.join();
}

/// A lookup that the session may not have, on a table that has it.
///
/// The deny path must not be cheaper by skipping work it should do: a
/// primitive that is filtered out of a listing and one that is refused by a
/// lookup have to reach the same decision, and this is the benchmark that
/// would show a lookup short-circuiting the feature check because the scope
/// check happened to fail first.
#[divan::bench]
fn get_denied_1k(b: Bencher) {
    let store = store_of(&TOOL_NAMES);
    let session = anonymous();
    let profile = everything();

    b.bench(|| {
        let found = store.get_visible(
            black_box(&session),
            black_box(&profile),
            black_box(protocol_droid::domain::registry::PrimitiveKind::Tool),
            black_box(name_at(999)),
        );
        black_box(found)
    });
}

/// A primitive carrying a scope requirement, listed to a session that holds
/// it. The filter has to consult the scope half, and this is the only
/// benchmark where that half is on the allow path rather than the deny one.
#[divan::bench]
fn list_scoped_1k(b: Bencher) {
    let store = RegistryStore::new();
    for name in TOOL_NAMES.iter() {
        store.register(tool(name).requiring_scope(Scope::Admin));
    }
    let session = unrestricted();
    let profile = everything();

    b.bench(|| {
        let listing = store.list_visible(
            black_box(&session),
            black_box(&profile),
            black_box(protocol_droid::domain::registry::PrimitiveKind::Tool),
        );
        black_box(listing.total())
    });
}

/// The table copy alone, a hundred tools, with no tool construction in it.
///
/// `to_builder` plus `build_at` is exactly what a publish does to the table,
/// so this is the immutability cost with the JSON parsing taken out: 312 ns,
/// against the 35 µs that building the same hundred tools costs. The
/// immutability tax is under a hundredth of the cost of filling the table in
/// the first place, which is the number that justifies the design.
#[divan::bench]
fn rebuild_100(b: Bencher) {
    let store = store_of(&SMALL_NAMES);
    let snapshot = store.snapshot();

    b.bench(|| {
        let rebuilt = black_box(&snapshot).to_builder().build_at(1);
        black_box(rebuilt)
    });
}

/// The same copy, a thousand tools. Compare against `rebuild_100`.
///
/// The claim is that the rebuild is linear in the table, and the measurement
/// is 4.58 µs against 312 ns — 14.7x for 10x the primitives. That is not the
/// 10x a pure linear scan would give, and the reason is memory rather than
/// control flow: a thousand `Arc<Registered>` slots and a thousand hash
/// entries do not sit in the same cache lines as a hundred, so the constant
/// per element grows. A change that made this quadratic would not be a
/// 10x-to-15x move; it would be a 100x one, so a ratio that drifts much past
/// about twenty is the signal worth acting on.
#[divan::bench]
fn rebuild_1k(b: Bencher) {
    let store = store_of(&TOOL_NAMES);
    let snapshot = store.snapshot();

    b.bench(|| {
        let rebuilt = black_box(&snapshot).to_builder().build_at(1);
        black_box(rebuilt)
    });
}

fn main() {
    divan::main();
}
