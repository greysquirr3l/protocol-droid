# Why one IR

The MCP specification has changed five times. The schema files for
those five revisions are vendored under
`crates/protocol-droid-schema/spec/<revision>/`. A naïve library
picks one revision at compile time, and every consumer pins the
revision they wrote their server against. A consumer that wants to
speak to a server on a newer revision is stuck: their types do not
exist for it.

`protocol-droid` resolves this with a single canonical *internal*
representation — the IR — and a per-revision *codec* on either side
of it. The IR is a superset of every revision; the codecs lift wire
messages into the IR and lower them back. Application code speaks the
IR; the wire version is something the negotiation step chose.

## What the IR is, and what it is not

The IR is **not** a model of "the latest MCP". It is a model that
holds the union of every revision's surface. A `2024-11-05` codec
populates a subset of the IR; a `2026-07-28` codec populates a
different subset, with `_meta` fields and stateless-lifecycle
directives the older one never heard of. The two never have to
agree on what a request looks like; they agree on what the
*internal* form looks like, and the codecs do the rest.

A consequence: an application that has to deal with a peer on
revision `X` writes against the IR, and a separate consumer that has
to deal with a peer on revision `Y` writes against the same IR. The
codecs for `X` and `Y` are different; the application code is the
same.

## What this is not for

This is not a "polyglot" claim. The library does not pretend that a
`2024-11-05` server and a `2026-07-28` client can interoperate at
the wire level. They cannot — the wire types genuinely differ. What
the library *does* do is give both peers a single API to speak *to*
the library, and a single API to drive *the library* with. The wire
compatibility is the bridge's problem, not the application's.

## A concrete look

A `tools/call` request is a JSON object. On `2024-11-05`, the
`params.arguments` field is `JsonObject` (stringly-typed). On
`2025-06-18`, the `params.arguments` is a typed schema. The IR has
neither of those shapes — it has the *application's* shape, a
typed struct the application defined. The `2024-11-05` codec
parses the `JsonObject` and tries to lift it into the typed
struct; if the lift fails, the codec returns a typed
`LowerError::UnknownField` and the application sees that, not a
"params was the wrong shape" error from serde.

This is what "the application speaks the IR" means in practice.
The wire shape is the codec's problem; the application never
sees it.
