# Vendored specification revisions

Every file under this directory is a verbatim copy of an official Model Context
Protocol specification artifact, pinned to an exact commit. Nothing here is
hand-written, and nothing here is generated from our own understanding of the
protocol.

If you are about to change a file in here by hand, you want
`cargo xtask refresh-spec` instead.

## Layout

```
spec/
├── MANIFEST.toml        Provenance and digests for every revision
├── 2024-11-05/          schema.json, schema.ts, CHANGELOG.md
├── 2025-03-26/
├── 2025-06-18/
├── 2025-11-25/
├── 2026-07-28/
├── draft/               Tracks upstream `main`; no tag exists yet
└── cited/               Normative documents the authorization sections cite
```

Each revision directory holds the same three files:

| File           | What it is                                                 | Digest-locked          |
| -------------- | ---------------------------------------------------------- | ---------------------- |
| `schema.json`  | The JSON Schema for the revision                           | yes                    |
| `schema.ts`    | The TypeScript source the JSON Schema is generated from    | yes                    |
| `CHANGELOG.md` | What changed in this revision relative to the previous one | yes, except 2024-11-05 |

`2024-11-05` is the first published revision, so it has no predecessor to
describe changes from. Its changelog is ours, not upstream's, and is
deliberately left unlocked so it can be edited without breaking verification.
That is the only file in the tree that is not a verbatim copy.

## `cited/` — the documents the authorization sections cite

A revision's schema says what an MCP _message_ looks like. It says nothing about
what a PKCE challenge looks like, what fields a dynamic client registration
request carries, or how a resource indicator is encoded — those live in RFCs and
one IETF draft. Since T16 implements the OAuth client half, "what exactly are we
coding against?" needs an answer for those too, and it does not belong in
anyone's memory.

So `cited/` holds the normative documents the authorization sections depend on,
verified by the same rules as a revision directory: every recorded document is
present with its recorded digest, and a document present but unrecorded fails
the build.

Three things make it different from `spec/<version>/`:

- **Not MCP artifacts.** Each file comes from its own publisher at the URL
  recorded beside it — `rfc-editor.org` for RFCs, the IETF archive for drafts.
  Nothing here is copied out of the MCP repository.
- **One directory, not one per revision.** RFC 7636 is cited by several
  revisions; a copy per revision would be N copies free to drift apart.
- **Pinned by URL and digest, not by commit.** There is no upstream git commit
  to pin. The digest is what makes the pin real, and re-fetching is an explicit
  command rather than something a build does.

The directory is called `cited/` rather than `rfc/` because one of its contents
is an Internet-Draft, not an RFC.

`cited_by` in the manifest records which revisions' authorization sections
require each document. It is a judgement about prose that is not vendored, so it
is recorded here to be read and argued with in review rather than recomputed by
code.

```bash
cargo xtask refresh-cited                 # all of them
cargo xtask refresh-cited rfc7636         # just one
```

## Why the manifest exists

The published schemas are the source of truth for every wire type this
toolkit generates, so "what exactly are we coding against?" needs an answer
that survives a year of refactoring. `MANIFEST.toml` records, per revision:

- the upstream repository and the raw-content base URL,
- the exact commit SHA the files were copied from,
- where that revision's files live upstream (`schema_prefix`),
- the SHA-256 of every file we vendored.

Three properties make it worth maintaining:

1. **Pinned by commit, never by branch.** A branch name is not a pin — it moves.
2. **Digest-locked.** A tampered or truncated vendored file fails the build.
3. **Reconciled both ways.** A file on disk that nothing records is rejected,
   not just a recorded file that is missing. Otherwise a smuggled file would go
   unverified while the manifest still looked complete.

## Verification

`build.rs` runs on every build of `protocol-droid-schema` and refuses to
compile if the tree disagrees with the manifest. The check is a gate, not a
suggestion: a build that vendors a schema it cannot verify is exactly the
failure mode this task exists to prevent.

`crates/protocol-droid-schema/tests/manifest_verification.rs` covers the same
ground as named tests you can read and run individually, plus the cases the
build script treats as fatal — a corrupted file, a smuggled file, a deleted
file.

The build never touches the network. That is deliberate: a build that silently
re-fetches a schema makes builds unreproducible and lets the wire types change
without a commit.

## Refreshing

```bash
cargo xtask refresh-spec 2026-07-28     # re-fetch a tagged revision
cargo xtask refresh-spec draft --newest  # re-resolve main, repin, and re-fetch
```

This downloads each file from the recorded commit, rewrites its digest, and
reports what changed:

```
refreshed 2025-06-18 at f5ccad944fdf2b7d9cc70cf817f66ca5a8aa03a4
  updated   schema.json  (upstream moved; manifest digest rewritten)
  unchanged CHANGELOG.md
  unchanged schema.ts
```

The three outcomes are distinct on purpose:

- **updated** — upstream moved. Both the file and its recorded digest change.
- **repaired** — the local copy was stale or corrupted but upstream had not
  moved. The file is rewritten; the manifest is left alone.
- **unchanged** — already identical. Nothing is written.

Without that distinction, a refresh would report "unchanged" while leaving a
corrupted file in place, because the download would match the recorded digest
and the comparison would never look at the file on disk.

Review `git diff` afterwards. The manifest and the vendored files must land in
the same commit: one without the other fails the build, which is the intended
behaviour and not something to work around.

### Adding a new revision

`refresh-spec` updates a revision; it does not invent one. To vendor a new
release:

1. Add a `[revisions.<version>]` table to `MANIFEST.toml` with `tag`,
   `commit`, `schema_prefix`, and `changelog`.
2. Create the directory: `mkdir -p spec/<version>`.
3. `cargo xtask refresh-spec <version>` to fetch the files and fill in digests.
4. If you are deliberately not vendoring something, record it under
   `[meta.excluded_tags]` with a reason. A recorded exclusion is a decision; a
   silent gap looks like an oversight.

### A note on the manifest file itself

The manifest is treated as source, not as a serialisation target. Comments in
it carry real information — why 2024-11-05 has no upstream changelog, which
tags were excluded and why, that `draft` is a placeholder. So
`refresh-spec` rewrites digests by line, in place, preserving every comment,
every space of column alignment, and every trailing note. Round-tripping the
file through a TOML serialiser would delete all of it.
