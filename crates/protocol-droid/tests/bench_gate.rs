//! The bench gate's *smoke test*.
//!
//! # What this test exists to do
//!
//! The committed bench baselines in `docs/benchmarks.md` are the
//! numbers the CI gate compares against. The gate itself is the
//! `bench` CI job; the smoke test this file carries is the
//! cheapest way to prove the gate is wired up at all.
//!
//! Specifically: this test measures the same shape of work the
//! `decode` bench measures (`tools/call` envelope decode) on a
//! hot loop, asserts the per-iteration cost is comfortably
//! under the bench baseline (5× the median), and would fail if
//! a regression made the per-iteration cost 5× worse. The
//! threshold is 5× rather than 1× so the test is stable across
//! CI machines, OSes, and kernel scheduling quirks; the
//! *actual* gate is 1.1× (the `>10% regression` rule in
//! `docs/benchmarks.md`), and the smoke test's looser
//! threshold is what makes a "the gate is working" failure
//! visible without the gate being so tight it flakes on
//! Monday.
//!
//! # What this test is not
//!
//! It is not a bench. The full `cargo bench` run is the source
//! of the committed baseline; this test only asserts the
//! baseline is *plausible* (a single decode is not 5× slower
//! than the median). The actual regression gate lives in
//! `.github/workflows/ci.yml`'s `bench` job, not here.

use std::time::Instant;

/// The frame the bench measures; the same envelope the
/// `decode_tools_call` bench exercises. The text is the exact
/// bytes the bench hands the decoder, so a change to the
/// test's text would invalidate the comparison.
///
/// # Why a raw string
///
/// The decoder is the boundary; the test is a
/// throughput measurement, and a hand-written
/// `serde_json::json!()` macro would re-serialise through
/// `serde_json::Value` and add a small but non-zero
/// amount of work the bench does not. The raw
/// string keeps the bytes identical to what a
/// real client would send.
const TOOLS_CALL_FRAME: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"add","arguments":{"a":2,"b":3}}}"#;

/// The committed baseline for `decode_tools_call` in
/// `docs/benchmarks.md` (median, 1.98.1, aarch64).
///
/// The smoke test runs in *dev* mode (no release profile,
/// no opt-level=3), and the dev/release cost ratio on
/// this code is ~15×. The 5× multiplier on top of the
/// committed release-mode number, plus a 100 ns padding
/// to absorb OS / kernel / cache variation across the
/// three CI runners, lands the threshold at:
///     200 ns (committed) * 5 (gate margin) * 15 (dev/release) + 100 ns
/// which is what the assertion below compares against.
/// A future commit that makes the decoder 5× slower
/// shows up here as a 5× regression against the
/// committed baseline, not against the dev-mode
/// smoke-test cost, so the test's threshold is
/// anchored to the *bench's* number rather than to
/// the test's own measurement.
const BASELINE_NS: u128 = 200;
const DEV_RELEASE_RATIO: u128 = 15;
const PAD_NS: u128 = 100;

/// How many iterations the smoke test runs. The number
/// is large enough that the elapsed wall time dwarfs
/// the per-iteration timing overhead (a `Instant::now()`
/// is ~25 ns on Linux) and small enough that the
/// test finishes in well under a second.
///
/// 100 000 iterations is the same order of magnitude
/// the divan bench uses, and a smoke test that
/// finishes in under a second is a smoke test a
/// developer will actually leave on.
const ITERATIONS: u64 = 100_000;

/// The smoke test. Runs the decoder on the
/// `tools/call` frame `ITERATIONS` times and asserts
/// the per-iteration cost is below 5× the committed
/// baseline.
///
/// # Errors
///
/// Returns `Err` when the gate would fail — i.e. when
/// the per-iteration cost is above the 5× threshold.
/// The `Err` carries the measured number so a
/// failing run prints "per-iter was N ns, threshold
/// is M ns" rather than just "regression".
#[test]
fn decode_is_within_bench_gate() -> Result<(), String> {
    // Warm the cache and the I-cache. The first call
    // loads the decoder's text from .text and the
    // dispatch table from .rodata; both are
    // deterministically paged, and the very first
    // iteration would otherwise be a cold-cache
    // measurement the bench never sees.
    let mut sink: u64 = 0;
    for _ in 0..1_000 {
        let outcome = protocol_droid::domain::jsonrpc::decode(TOOLS_CALL_FRAME.as_bytes());
        sink = sink.wrapping_add(u64::from(outcome.is_ok()));
    }

    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let outcome = protocol_droid::domain::jsonrpc::decode(TOOLS_CALL_FRAME.as_bytes());
        sink = sink.wrapping_add(u64::from(outcome.is_ok()));
    }
    let elapsed = started.elapsed();
    let per_iter = elapsed.as_nanos() / u128::from(ITERATIONS);

    // `sink` is a side-channel to prevent the optimiser
    // from eliding the entire loop. A `Result` is
    // ~32 bytes; touching `sink` is what the optimiser
    // notices when it walks the post-loop DAG. The
    // exact value is irrelevant; the test only
    // needs the loop's body to stay live.
    std::hint::black_box(sink);

    let threshold = BASELINE_NS * 5 * DEV_RELEASE_RATIO + PAD_NS;
    if per_iter > threshold {
        return Err(format!(
            "decoder per-iter was {per_iter} ns, threshold is {threshold} ns (5x the committed baseline of {BASELINE_NS} ns, scaled for dev-mode); this is the kind of regression the bench gate would catch"
        ));
    }
    Ok(())
}
