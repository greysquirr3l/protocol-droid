---
title: Spec refresh process
description: How a new MCP spec revision becomes a row in the feature matrix, what the review surface looks like, and the gates that catch a mistake.
---

A new MCP spec revision becomes a row in the matrix through a
sequence of mechanical steps. The goal is to keep the human
review focused on the *new* things the revision introduces or
removes, and to let the rest be exercised by the conformance
suite and the interop tests.

## 1. Vendor the schema

`crates/protocol-droid-schema/spec/<version>/` is the
authoritative source. The vendored files are the `schema.json`
and `schema.ts` from the official MCP repo, unchanged. The
schema crates' codegen reads from this directory and is the
only consumer; nothing hand-writes wire types that the
codegen produces.

## 2. Run the schema codegen

`cargo build -p protocol-droid-schema` regenerates the
per-revision Rust types from the vendored schema. The diff
the PR shows is the *Rust* delta for the new revision; a
human review of the schema delta is the right place to
catch a codegen bug.

## 3. Update the feature matrix

`crates/protocol-droid/src/domain/feature.rs` is the
matrix. The new revision's `CapabilitySet` is added to the
`all_for` match, and any new features gain a row in the
matrix with a citation to the spec changelog section that
introduced or removed them.

:::caution
A row without a citation is a failing task. The matrix
is the crate's central claim, and a claim with no
provenance is a claim nobody can check.
:::

## 4. Re-record the snapshot

`cargo insta test` rewrites
`crates/protocol-droid/tests/snapshots/feature_matrix_golden__matrix_<version>.snap`
with the new row. The diff is the matrix update as a
reviewer reads it: every revision, every transport, every
feature, in one file.

## 5. Run the conformance suite

`cargo run -p protocol-droid-cli -- conformance server
--url http://127.0.0.1:8080/mcp` exercises the suite
against the new revision's reference server. A green
run is the matrix's external proof; a red row is the
matrix's gap.

## 6. Add the new version to the negotiate test

`crates/protocol-droid/tests/negotiation.rs` has a row
per version. A new row that picks the new revision on
both sides and asserts the resulting `FeatureProfile`
is the matrix's runtime proof.

## What this is not

This is not a "minimum surface" change. A new revision
*adds* features and may *remove* others; the matrix
must reflect both. The snapshot is the diff, the
conformance run is the proof, and the negotiation test
is the gate.
