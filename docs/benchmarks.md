# Performance baselines

Performance is the second of the project's three goals (after
correctness, before dynamism). The numbers in this file are the
**committed baselines** the CI regression gate compares against;
re-running the benches locally and committing the result is the
way the baseline moves.

## Toolchain

All bench numbers in this file were measured on:

| Component | Version |
| --- | --- |
| `rustc` | `1.98.1` (`rustc 1.98.1 (48a229cea 2026-09-01)`) |
| `cargo` | same toolchain |
| `divan` | `0.1.21` |
| OS | macOS 26 (Apple silicon), single-threaded benches unless noted |

The pin is recorded in [`rust-toolchain.toml`](../rust-toolchain.toml)
and matched by `RUST_TOOLCHAIN` in
[`.github/workflows/ci.yml`](../.github/workflows/ci.yml). A bench
session on a different compiler is **not comparable** to the
committed numbers; rustc's optimiser and inliner choices shift
frequently between releases, and a number that looks like a
regression is often a compiler upgrade in disguise.

## How the gate works

There are two things here, and it is worth being precise
about which of them is automated.

**The smoke test is the automated gate.** It runs in CI, on
every push, as part of `cargo test`. It measures the decode
path and fails if the decoder has become grossly slower. It
is described under [Smoke test](#smoke-test) below.

**The divan benches are not currently run by CI.** The three
bench targets in `crates/protocol-droid/benches/` exist and
are maintained, and their medians are committed in
[`baselines.json`](../crates/protocol-droid/benches/baselines.json),
but no CI job runs them and no tool compares a run against
the committed numbers. The tables below are reference data
for a human reading a bench run, not a gate.

This was true of the previous revision of this document
too, which described a CI bench job comparing per-bench
medians against `baselines.json` with a 10 % threshold in
either direction. That job does not exist, and the
description of it was aspirational.

Why it was never wired up is worth recording, because it is
the reason to be careful about wiring it up naively: a 10 %
threshold on a shared CI runner is not a reliable signal.
The smoke test's own history is the demonstration — it
measured the *same* code at 4 300 ns per iteration on an
idle machine and 59 563 ns on a busy one, because a
wall-clock sample measures the machine as much as the code.
A 10 % gate would have been far worse. Putting the divan
benches in CI needs a runner that is not shared, and a
comparison that understands the difference between a
regression and a noisy runner. That is a piece of work in
its own right, not a wiring exercise.

The threshold this project *would* use is 10 %, symmetric on
purpose: a benign compiler upgrade that makes the code 20 %
faster is as much a sign that the bench is no longer
measuring what it should as a regression in the other
direction. A 10 % threshold is wide enough to absorb noise
from cache state, CPU thermals, and the kernel's scheduling
decisions, and tight enough that a `Vec::extend` rewritten
as `vec![..; n]` shows up. It is not negotiable per PR; the
right response to a legitimate gain is to commit the new
baseline, not to widen the gate.

## Benches

### `jsonrpc_hot_path` — [`crates/protocol-droid/benches/jsonrpc_hot_path.rs`](../crates/protocol-droid/benches/jsonrpc_hot_path.rs)

The envelope, the router, and the id path. The numbers in
this bench are the project's perf-critical claim made
concrete; every JSON-RPC frame in the system passes through
`decode` once and through `route_one` exactly when the
router has a registered handler for the method.

The committed baseline for each bench row lives in
[`crates/protocol-droid/benches/baselines.json`](../crates/protocol-droid/benches/baselines.json);
the table below renders a representative subset for
reading.

| Bench | What it measures | Baseline (median) |
| --- | --- | --- |
| `decode_tools_call` | a single `tools/call` envelope | **180.7 ns** |
| `decode_string_id` | an envelope with a string id, escaped on the wire | **129.3 ns** |
| `decode_escaped_id` | an envelope with an escaped id (the 6× slower path) | **749.7 ns** |
| `decode_numeric_id` | an envelope with a numeric id | **94.5 ns** |
| `decode_batch` | a 4-element batch | **244.5 ns** |
| `a_known_method_routes` | the router against the real MCP table | **171.6 ns** |
| `an_unroutable_frame_never_reaches_the_params` | the cost of a method name not in the table, byte-for-byte the same params as `a_known_method_routes` | **183.3 ns** |
| `a_batch_routes_every_element` | the batch case against the real MCP table | **291.7 ns** |

The two routing rows are deliberately close in the committed
baseline: 171.6 ns vs 183.3 ns, a 7 % gap. A change that
parsed params before looking the name up would show up as
the unroutable row drifting *above* the routable row by a
whole table's worth of work; the bench would catch that
before review does.

### `registry` — [`crates/protocol-droid/benches/registry.rs`](../crates/protocol-droid/benches/registry.rs)

The dynamic registry's read and write paths, at 100 and 1 000
tools. The 1 000-tool shape is the one a server that
aggregates three upstream servers carries; the 100-tool shape
is the ordinary single-server case. The two scales are kept
separate so a regression in either shows up on its own.

| Bench | What it measures | Baseline (median) |
| --- | --- | --- |
| `list_1k` | a one-pass read at 1 000 tools | **2.957 µs** |
| `get_1k` | a single-tool lookup at 1 000 tools | **34.5 ns** |
| `rebuild_1k` | a publish at 1 000 tools | **5.04 µs** |
| `rebuild_100` | a publish at 100 tools | **353.6 ns** |
| `build_1k` | a 1 000-tool initial population | **366.8 µs** |
| `publish_1k` | a registry publish with a concurrent reader | **9.37 µs** |
| `unpublish_1k` | a registry unpublish at 1 000 tools | **12.35 µs** |

The `rebuild_1k` vs `rebuild_100` ratio is 14.3× — exactly
the linear-in-table shape the snapshot copy is supposed to
have. A change that made `rebuild_1k` quadratic in the
table would show up as that ratio jumping by an order of
magnitude; a change that broke the linear shape would show
up before review does.

## Filling in the baselines

The next session runs:

```sh
cargo bench -p protocol-droid --bench jsonrpc_hot_path
cargo bench -p protocol-droid --bench registry
```

and commits the median of the per-bench output as the new
baseline row. The bench framework caches its result files
under `target/criterion/...`; the committed file is the
*parsed* median, not the raw output, so a reader does not
have to run a bench to know what number the gate is
comparing against.

## Smoke test

[`crates/protocol-droid/tests/bench_gate.rs`](../crates/protocol-droid/tests/bench_gate.rs)
is the automated performance gate. It runs the same
`decode` shape the bench exercises on a 100 000-iteration
hot loop, asserts the per-iter cost is below 5× the
committed `decode_tools_call` baseline (scaled for
dev-mode), and fails if a regression made the decoder 5×
slower. It runs in under a second and is what the
developer's `cargo test` turns up.

It is a 5× gate and not a 10 % one, on purpose. A tighter
threshold on a wall-clock measurement is not a stricter
gate, it is a noisier one: the same decoder measured 4 300
ns per iteration on an idle machine and 59 563 ns on a
busy one, so a 10 % threshold would fire on the machine
rather than on the code.

Two details make it reliable on a shared runner:

- **The measurement is the fastest of ten samples, not the
  mean.** The fastest sample is the one least disturbed by
  the OS preempting the test thread. A real regression is
  present in every sample, so the minimum still catches it.
  Measured under a load average of 20, the fastest of ten
  samples was 4 640 ns while the median was 12 179 ns and
  the slowest was 22 079 ns — the minimum recovers the
  uncontended cost, the median does not.
- **The threshold is anchored to the committed release-mode
  baseline**, not to the smoke test's own measurement. It
  scales by the dev/release cost ratio (measured at ~21×),
  so the assertion means "5× slower than the bench's
  number" rather than "5× slower than whatever this machine
  did today".

A second test in the same file pins that contract: it asks
the gate's own comparison whether a synthetic 5× slower
decoder trips it, and whether an uncontaminated one does
not. Without it the constants could be loosened until the
gate stopped catching anything, and nothing would say so —
the failure mode of a gate that has quietly stopped gating
is silence.

## Comparative bench vs rmcp

[`compare`](../crates/protocol-droid/tests/interop_rmcp/src/bin/compare.rs)
in the interop crate drives both servers with the **same** client (a
`protocol-droid` `StdioClient`), after the same handshake, so the only thing
that differs between the two runs is the server on the other end of the pipe.
It makes 200 warm-up calls and then N sequential `tools/call` requests on one
connection, and prints the median, 90th and 99th percentile, and calls per
second.

```text
cargo build --release --example stdio_server -p protocol-droid
cargo run --release --manifest-path crates/protocol-droid/tests/interop_rmcp/Cargo.toml \
    --bin compare -- --calls 8000
```

Three runs of 8 000 calls on an Apple M3 Pro, rustc 1.98.1, `rmcp 3.5.1`:

| Server           | p50 (µs) | p90 (µs) | p99 (µs) | calls/s         |
| ---------------- | -------: | -------: | -------: | --------------- |
| `protocol-droid` |       29 |    32–33 |    37–49 | 33 100 – 33 900 |
| `rmcp`           |       30 |    33–34 |    38–42 | 31 900 – 32 500 |

That is parity, with `protocol-droid` about 4 % ahead on throughput. It should
not be read as more than that. The workload is one connection, one request in
flight, a trivial tool, over a pipe on one machine: it says how much a server
adds to a round trip, and nothing about concurrency, large payloads, many
tools, or HTTP. The two tools differ (`shout` and `add`), though both return a
few dozen bytes. A shorter run (3 000 calls) once read 33 µs against 41 µs; the
longer runs did not reproduce that, so short runs on a laptop are noise at this
scale.

The first version of this harness also found a real bug: the server's
idempotency store refused every `tools/call` after its 4 096th, because it
filled with finished results and never made room. It now drops the oldest
finished result instead.
