# Conformance gaps

The official `@modelcontextprotocol/conformance` suite runs the
server-scenarios against [`crates/protocol-droid/examples/conformance_reference_server.rs`](../crates/protocol-droid/examples/conformance_reference_server.rs)
through `protocol-droid conformance self`. The matrix
(pinned suite `0.2.0-alpha.12` × four spec revisions) currently lands
at the per-revision pass rates below. The row "Suite" is what the
suite's own tooling reports. "Spec gap" and "Fixture gap" are *derived*
from the reason each scenario failed, not reported by the suite, and the
derivation has a known blind spot:

> **A scenario that failed because the method did not exist was counted as
> a fixture gap.** The suite does not distinguish "this server has no such
> tool" from "this server does not implement the method at all" — both are a
> non-2xx reply with a JSON-RPC error. So the "Fixture gap" column is an
> upper bound and the "Spec gap" column is a lower bound; the two do not sum
> to the failed total, and they cannot both be right.

That was not hypothetical. Until `2fcb9d6` the dispatcher answered seven
methods and refused everything else, so `resources/read` and `prompts/get`
returned `UnknownMethod` — a spec gap — and were counted as fixture gaps,
because the reference server also had nothing to hand back. Those methods are
served now, and the reader requirement in `56fb180` means a fixture can no
longer register a listing it cannot serve, which is what made the two counts
hard to tell apart in the first place.

**The matrix above predates both commits and has not been re-measured.** The
suite is not runnable from the environment these changes were made in, so the
`Spec gap` / `Fixture gap` split still reflects the old failure set. Re-run it
before trusting either column.

| Revision       | Passed | Failed | Total | Spec gap | Fixture gap |
| -------------- | -----: | -----: | ----: | -------: | ----------: |
| `2025-03-26`   |      — |      — |     — |        — |           — |
| `2025-06-18`   |     34 |     21 |    55 |        0 |          21 |
| `2025-11-25`   |     40 |     24 |    64 |        2 |          22 |
| `2026-07-28`   |      1 |     38 |    39 |       14 |          24 |

`2025-03-26` is not exercised by the matrix because the pinned suite
version does not include a `2025-03-26` scenario set; that exclusion
is documented in
[`crates/protocol-droid-cli/conformance/ALLOWLIST.md`](../crates/protocol-droid-cli/conformance/ALLOWLIST.md).

## Root cause of the fixture gap

The reference server today implements two tools (`echo`, `add`) and
two prompts (`review`, `summarize`). The suite's server scenarios
expect a larger surface — at least:

- **Content type tools.** `test_simple_text`, `test_image_content`,
  `test_audio_content`, `test_embedded_resource`,
  `test_multiple_content_types` — one per content shape the wire
  spec describes. Each one is a single tool whose `CallToolResult`
  carries a `content` array of the corresponding block. Today the
  reference server returns the wrong content type (or an "unknown
  tool" error), and the suite reports `1 passed, 1 failed` — the
  passing leg is the round-trip itself, the failing leg is the
  content shape.
- **Behavioural tools.** `test_logging_tool`, `test_tool_with_logging`,
  `test_tool_with_progress`, `test_sampling`, `test_elicitation`,
  `test_error_handling`, `test_reconnection`,
  `test_trigger_tool_change`, `test_trigger_prompt_change`,
  `test_headers`, `test_custom_headers`, `test_custom_headers_null`,
  `test_missing_capability`. Each one exercises a different side
  channel of the server: progress tokens, log messages, server
  notifications, the sampling callback, the elicitation callback,
  the error envelope. The reference server has none of them.
- **Tasks.** `test_tool_with_task`, `test_input_required_result_*`
  (seven variants), `test_mrtr_*` (five variants). Tasks are the
  `2025-11-25` asynchronous-tool surface, and the reference server
  has no `tasks` capability.
- **Prompts.** The suite expects `test_prompt`, `test_simple_prompt`,
  `test_prompt_with_arguments`, `test_prompt_with_embedded_resource`,
  `test_prompt_with_image` — the reference server's `review` and
  `summarize` are two of those, but the resource- and image-bearing
  variants are missing.
- **Resources.** `test://static-text`, `test://static-binary`,
  `test://template/.../data`, `test://watched-resource` — the
  reference server has no resources.

The "1 passed, 38 failed" pattern on `2026-07-28` is two layers of
gap stacked: the same fixture gap as the older revisions, plus
the deterministic-order check on `tools/list` (introduced in
`2026-07-28`, SHOULD) and the tool-name-format check (introduced
in `2025-11-25`, SHOULD). The SHOULD-not-MUST status is the
difference between "1 passed" and "0 passed" — the SHOULD
checks are not assertion-grade in the matrix, only the MUST
behaviours are.

## Root cause of the spec gap

`2025-11-25` introduces two spec gaps this crate has not yet
implemented:

- `elicitation-sep1034-defaults` — the suite checks the
  `elicitation/create` request shape against the SEP-1034
  defaults. The crate's `elicitation` callback exists, but
  the request body is not yet typed against the SEP-1034
  extension.
- `elicitation-sep1330-enums` — the same callback against the
  SEP-1330 enum-elicitation extension.

`2026-07-28` introduces the wider set, but the dominant failure
mode on that revision is still the fixture gap (the reference
server returns 2-of-3 required tools, and `tools/list` reports
in deterministic order only by accident). Closing the fixture
gap on `2025-06-18` is the prerequisite for the same fix
landing on `2026-07-28`.

## Plan

The cheapest path to a green matrix is fixture-only: the
`#[server]` / `#[tool]` / `#[prompt]` / `#[resource]` macros
already support every shape the suite asks for, and the
`Content` and `CallToolResult` types are the wire shapes the
suite reads. The work is additive — a new tool, prompt, or
resource lands in one commit, the matrix reruns, and the
failing-leg count drops by one.

Order of operations (each row is one PR-sized chunk, with
its own matrix pass and a "X passed, Y failed" delta in
the commit body):

1. The five content tools (`test_simple_text`,
   `test_image_content`, `test_audio_content`,
   `test_embedded_resource`, `test_multiple_content_types`).
   This alone takes `2025-06-18` from 34/55 to 39/55 and
   `2025-11-25` from 40/64 to 45/64.
2. The two missing prompts (`test_prompt_with_embedded_resource`,
   `test_prompt_with_image`) and the three missing resources
   (`test://static-text`, `test://static-binary`,
   `test://template/.../data`). +6 to `2025-06-18`, +6 to
   `2025-11-25`.
3. The four capability tools (`test_logging_tool`,
   `test_tool_with_logging`, `test_tool_with_progress`,
   `test_error_handling`). +4 to both revisions.
4. The two sampling/elicitation tools (`test_sampling`,
   `test_elicitation`) and the two notification tools
   (`test_trigger_tool_change`, `test_trigger_prompt_change`).
   The handlers are stubs that emit the right notification; the
   suite checks the wire, not the behaviour. +4 to both
   revisions.
5. The `tasks` capability and its six or so tools. This is
   the actual work: a `2025-11-25` task is a server-side
   long-running operation the consumer polls, and the suite
   is the first end-to-end check this crate has had for it.
   The `task` domain types exist; the surface on the wire
   does not yet. +0 to `2025-06-18`, +N to `2025-11-25` and
   `2026-07-28`.
6. The two SEP-1034/SEP-1330 elicitation extensions, as a
   crate-level feature flag (they are optional extensions, not
   base spec). +0 to `2025-06-18`, +2 to `2025-11-25` and
   `2026-07-28`.

The two spec gaps on `2026-07-28` (deterministic-order,
tool-name-format) are SHOULDs and the suite reports them
as warnings, not failures — they appear in the "1 passed"
half of each row and are not on the critical path.

## What this is not, and what it once claimed to be

This was previously written as: *this is a fixture problem, not a library
problem. The codecs and the session engine round-trip every wire shape the
suite checks.*

That second sentence was false while it stood, and the falsity is worth
recording. `resources/read` and `prompts/get` were not round-tripped at all —
they were refused by the dispatcher before any codec saw them. The "passing
leg" of a content-shape scenario proved the scenario reached the server and
got a well-formed error back, not that the crate understood the message. A
handful of well-formed errors and a handful of well-formed results look
identical from the suite's side, which is exactly why the counting note at the
top of this file is in terms of failure *reason* and needs re-measuring.

The narrower claim is the one that holds: the suite's assertion on the
content-shape scenarios is about what the *reference server* returns, and
that server is what implements those shapes. Closing the fixture gap closes
the matrix. It should not be read as evidence that no library gap remains —
it is evidence that this particular measurement could not see one.
