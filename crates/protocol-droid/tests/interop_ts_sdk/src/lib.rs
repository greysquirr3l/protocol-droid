//! Differential interop between `protocol-droid` and the official
//! TypeScript MCP SDK (`@modelcontextprotocol/sdk`).
//!
//! # What this crate exists to do
//!
//! The rmcp interop crate (T22's other leg) cross-tests against the
//! Rust SDK most consumer projects will reach for; this one cross-tests
//! against the SDK the official `conformance` suite itself uses
//! under the hood, and which every consumer integrating through
//! Node or a browser is the SDK they will reach for. A wire bug
//! between `protocol-droid` and the TypeScript SDK is a bug a
//! consumer hits in the first hour.
//!
//! # What this crate is not
//!
//! It is not a replacement for the conformance suite. The conformance
//! suite reads the spec, and a finding is a *spec citation*; this crate
//! reads the *other SDK*, and a finding is an *SDK mismatch*. The two
//! are different investigations: a difference between the TS SDK and
//! `protocol-droid` is a bug in one of them, but a difference between
//! either of them and the spec is a bug in *both*. T22 surfaces the
//! first class of finding and feeds it back into T21's allow-list or
//! the TS SDK's issue tracker.
//!
//! # The matrix
//!
//! Each test exists in two directions:
//!
//! 1. `protocol-droid` client → TS SDK server (the TS server is
//!    `fixtures/server.mjs`, spawned by the test as a child process
//!    and driven over stdio)
//! 2. TS SDK client → `protocol-droid` server (the TS client is
//!    `fixtures/client.mjs`, spawned by the test with
//!    `protocol-droid`'s `stdio_server` example as its child; the
//!    client drives the conversation and prints a single JSON
//!    summary to stdout that the Rust test parses)
//!
//! and across the revisions both libraries agree on. The cross-test
//! is only meaningful for revisions *both* SDKs implement; the
//! matrix's first column is therefore the intersection.
//!
//! # How the TS SDK is fetched
//!
//! `fixtures/package.json` pins `@modelcontextprotocol/sdk` at
//! `1.32.1` and the test crate's setup step runs `npm install` to
//! populate `fixtures/node_modules/`. A `fixtures/package-lock.json`
//! is committed so a CI runner reproduces the same lockfile
//! resolution a local dev gets.
//!
//! # How it is wired into CI
//!
//! `.github/workflows/interop.yml` runs `cargo test -p interop-ts-sdk`
//! on the same matrix legs that run the rmcp interop crate. A
//! regression in either direction fails the run, and the diff is the
//! failing test's output.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

/// The protocol revisions the matrix exercises.
///
/// The intersection of what `@modelcontextprotocol/sdk` `1.32.1`
/// implements and what `protocol-droid` claims. As of the pin, the
/// TS SDK's `SUPPORTED_PROTOCOL_VERSIONS` lists
/// `2025-11-25` (latest), `2025-06-18`, `2025-03-26`,
/// `2024-11-05` and `2024-10-07`. `protocol-droid` does not
/// claim `2024-10-07` (a pre-release draft the project never
/// vendored), and the TS SDK does not yet implement
/// `2026-07-28`'s SEP-2575 stateless lifecycle. The
/// intersection is therefore the four revisions the test
/// matrix iterates over. A bump in the TS SDK's supported
/// set should bump this list in lockstep, and the cross-test
/// in `pd_client_to_ts_server_matrix` is what proves the bump
/// is real.
pub const REVISIONS: &[&str] = &[
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
    "2025-11-25",
];

/// A small shared error type the tests return.
///
/// One enum with a `From` per underlying error so `?` reaches the
/// right context. A test that swallows a broken fixture and then
/// asserts on the result is worse than one that does not compile, and
/// `From<anyhow::Error>` is the difference.
#[derive(Debug)]
pub enum Failure {
    /// The TS SDK rejected something the cross-test sent.
    TsSdk(String),
    /// `protocol-droid` rejected something the cross-test sent.
    ProtocolDroid(String),
    /// The child process could not be spawned, killed, or waited on.
    Process(String),
    /// The cross-test's own scaffolding was wrong.
    Harness(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TsSdk(message) => write!(f, "ts-sdk: {message}"),
            Self::ProtocolDroid(message) => write!(f, "protocol-droid: {message}"),
            Self::Process(message) => write!(f, "process: {message}"),
            Self::Harness(message) => write!(f, "harness: {message}"),
        }
    }
}

impl std::error::Error for Failure {}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self::Harness(format!("{error:#}"))
    }
}

/// A type alias the test functions return to keep the `Result<T, _>`
/// signatures short without losing the `From` conversion.
pub type Result<T> = core::result::Result<T, Failure>;
