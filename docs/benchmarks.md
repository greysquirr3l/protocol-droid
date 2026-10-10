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

The CI bench job runs the same two benches this file records
and compares the per-bench medians against the committed
`baselines.json`. A move of more than **10 %** in either
direction fails the run. The gate is symmetric on purpose: a
benign compiler upgrade that makes the code 20 % faster is as
much a sign that the bench is no longer measuring what it
should as a regression in the other direction.

A 10 % threshold is wide enough to absorb noise from cache
state, CPU thermals, and the kernel's scheduling decisions,
and tight enough that a `Vec::extend` rewritten as
`vec![..; n]` shows up. The threshold is not negotiable per
PR; the right response to a legitimate gain is to commit the
new baseline, not to widen the gate.

## Benches

### `jsonrpc_hot_path` — [`crates/protocol-droid/benches/jsonrpc_hot_path.rs`](../crates/protocol-droid/benches/jsonrpc_hot_path.rs)

The envelope, the router, and the id path. The numbers in
this bench are the project's perf-critical claim made
concrete; every JSON-RPC frame in the system passes through
`decode` once and through `route_one` exactly when the
router has a registered handler for the method.

| Bench | What it measures | Baseline (median) |
| --- | --- | --- |
| `decode` | a single `tools/list` envelope | _to be filled in next session_ |
| `route_one` | a known-method route through the real MCP table | _to be filled in next session_ |
| `an_unroutable_frame_never_reaches_the_params` | the cost of a method name not in the table, byte-for-byte the same params as `route_one` | _to be filled in next session_ |

### `registry` — [`crates/protocol-droid/benches/registry.rs`](../crates/protocol-droid/benches/registry.rs)

The dynamic registry's read and write paths, at 100 and 1 000
tools. The 1 000-tool shape is the one a server that
aggregates three upstream servers carries; the 100-tool shape
is the ordinary single-server case. The two scales are kept
separate so a regression in either shows up on its own.

| Bench | What it measures | Baseline (median) |
| --- | --- | --- |
| `list_1k` | a one-pass read at 1 000 tools | _to be filled in next session_ |
| `get_1k` | a single-tool lookup at 1 000 tools | _to be filled in next session_ |
| `rebuild_1k` | a publish at 1 000 tools | _to be filled in next session_ |
| `build_1k` | a 1 000-tool initial population | _to be filled in next session_ |

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

## Comparative bench vs rmcp

Not yet filled in. The shape is: an identical workload
(`tools/list` against a 1 000-tool registry, then
`tools/call add` 1 000 times) driven by the same harness
on top of `rmcp 3.5.1` and on top of `protocol-droid`,
with the two numbers in the same row of the same table.
The harness lives in
[`crates/protocol-droid/tests/interop_rmcp`](../crates/protocol-droid/tests/interop_rmcp/)
once it is wired up; the comparison here is what the next
session produces.
