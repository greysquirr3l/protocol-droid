//! Differential interop between `protocol-droid` and the official Rust MCP
//! SDK (`rmcp`).
//!
//! # What this crate exists to do
//!
//! The conformance suite (T21) is the *one* SDK the project tests against
//! directly, but rmcp is the SDK most consumer projects will reach for,
//! and an interop bug between protocol-droid and rmcp is a bug a
//! consumer hits in the first hour. T22 is the cross-test: the same
//! scenarios the conformance suite asserts, asserted *here* between this
//! crate's two clients and the other's two servers.
//!
//! # What this crate is not
//!
//! It is not a replacement for the conformance suite. The conformance
//! suite reads the spec, and a finding is a *spec citation*; this crate
//! reads the *other SDK*, and a finding is an *SDK mismatch*. The two
//! are different investigations: a difference between rmcp and
//! protocol-droid is a bug in one of them, but a difference between
//! either of them and the spec is a bug in *both*. T22 surfaces the
//! first class of finding and feeds it back into T21's allow-list or
//! rmcp's issue tracker.
//!
//! # The matrix
//!
//! Each test exists in two directions:
//!
//! 1. `protocol-droid` client → `rmcp` server (spawned as a child
//!    process from this crate's `rmcp_stdio_server` binary)
//! 2. `rmcp` client → `protocol-droid` server (spawned from
//!    `protocol-droid`'s `stdio_server` example)
//!
//! and across the revisions both libraries agree on. The cross-test is
//! only meaningful for revisions *both* SDKs implement; the matrix's
//! first column is therefore the intersection.
//!
//! # How it is wired into CI
//!
//! `.github/workflows/conformance.yml` runs `cargo test -p interop-rmcp`
//! on the same matrix legs that run the conformance suite. A
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
/// The intersection of what `rmcp` 3.5.1 implements and what
/// `protocol-droid` claims. As of the pin, `rmcp` 3.5.1 negotiates
/// down to `2025-11-25` when asked for `2026-07-28`, so the
/// intersection is what both libraries *actually agree on*,
/// not what each library's docs claim. A bump in `rmcp`'s
/// supported set should bump this list in lockstep, and the
/// cross-test in `pd_client_to_rmcp_server_matrix` is what
/// proves the bump is real.
pub const REVISIONS: &[&str] = &[
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
    /// `rmcp` rejected something the cross-test sent.
    Rmcp(String),
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
            Self::Rmcp(message) => write!(f, "rmcp: {message}"),
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
