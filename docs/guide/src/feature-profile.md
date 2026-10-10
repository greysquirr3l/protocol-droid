# FeatureProfile and gating

The `FeatureProfile` is a bitset of capabilities a peer *actually
negotiated*, derived from four inputs:

1. the protocol version,
2. the client's capabilities,
3. the server's capabilities,
4. the transport.

A pure domain function in `protocol_droid::domain::feature` maps
those four to the `FeatureProfile`. Every feature-specific code path
consults the profile before it runs. The profile is the source of
truth; the capabilities the peer advertised are inputs to it.

## Why this is not a runtime check

A peer that does not advertise `tools.list_changed` cannot receive
a `notifications/tools/list_changed`. A peer that does not advertise
`resources.subscribe` cannot receive a `resources/updated`. The
peer's capabilities are the contract; the wire code that *would*
emit a notification a peer cannot receive is a wire code that
should not run.

The library enforces this by consulting the `FeatureProfile` at the
boundary where a feature-specific path would otherwise fire. The
two outcomes are:

- **Refuse with a typed error.** The peer did not advertise
  the feature; the request is rejected with a `ProtocolError` that
  names the missing capability. This is the default.
- **Degrade under an explicit `DegradePolicy`.** The caller has
  opted in to a documented, lossy-or-flagged downgrade. A
  `DegradePolicy::Skip` on a `list_changed` notification drops
  the notification; a `DegradePolicy::Annotate` on a tool result
  adds an `_meta` flag the caller can inspect.

The library does not silently drop a notification or swallow an
error. A `DegradePolicy` is the explicit opt-in to do either, and
the policy is part of the call site, not part of the codec.

## How to read the matrix

The matrix is in `crates/protocol-droid/src/domain/feature.rs`. The
shape is:

- **Rows** are features (`Feature::Tools`, `Feature::StreamableHttp`,
  ...).
- **Columns** are protocol versions.
- **Cells** are gated or not, depending on the version.

The matrix is also rendered as a snapshot in
`crates/protocol-droid/tests/feature_matrix_golden.rs`. The
snapshot is the human-readable form, and any change to the matrix
moves the snapshot, which is what surfaces a regression in code
review rather than at runtime.

## An example

```text
use protocol_droid::domain::{CapabilitySet, Feature, FeatureProfile, ProtocolVersion};

let profile = CapabilitySet::all_for(ProtocolVersion::V2025_06_18)
    .with_client(&client_capabilities)
    .with_server(&server_capabilities)
    .resolve(ProtocolVersion::V2025_06_18, TransportKind::StreamableHttp);

if profile.is_enabled(Feature::ToolStructuredOutput) {
    // The peer can carry `structuredContent`; lift the result
    // into the typed form.
} else {
    // The peer cannot; the result is `JsonObject`-shaped, and
    // any attempt to read it as typed will fail with a typed
    // `LowerError::UnknownField` rather than a serde error.
}
```

(The example is shown as text rather than as a
`rust`-highlighted block because the guide's compile
context does not link the protocol-droid library; the
[`feature_matrix_golden` snapshot](../crates/protocol-droid/tests/feature_matrix_golden.rs)
is the executable form.)

`is_enabled` is `const fn`-shaped in the hot path. The compiler
will fold the check at the call site, and the `if` branch is a
single branch on a bitset bit.
