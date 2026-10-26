//! The `version` field on `#[server]`, in a crate that is not `protocol-droid`.
//!
//! # Why this file is here rather than in `tests/macros.rs`
//!
//! The claim under test is *where* the version comes from. A server that
//! names no version reports the version of the crate that wrote its
//! `#[server]` attribute — and inside `tests/macros.rs` that crate *is*
//! `protocol-droid`, so the default and the wrong answer are the same
//! string. A test there cannot tell the two apart.
//!
//! A trybuild pass case is a separate crate: trybuild builds it as its own
//! package, at version `0.0.0`, against `protocol-droid` by path. The two
//! versions differ, so a server here that reports `0.0.0` is reporting the
//! *consumer's* version, and one that reported `0.5.3` would be reporting
//! the dependency's.
//!
//! # What it pins
//!
//! Three things, in order:
//!
//! 1. The default — no `version` at all — is the consuming crate's version.
//! 2. An explicit `version = env!("CARGO_PKG_VERSION")` is the same value,
//!    so a consumer can say where its version comes from without writing it
//!    twice.
//! 3. A literal is a version too, for a server whose version is not in a
//!    manifest.
//!
//! A trybuild pass case succeeds when it compiles *and* its `main` does not
//! panic, so the assertions here are the test. A failed assertion fails the
//! build, and a failed build fails the case.

/// A server that names no version at all.
struct Implicit;

/// A server that points at its own manifest.
struct Explicit;

/// A server whose version is written out.
struct Literal;

/// The version of the crate that wrote these attributes.
///
/// `env!` is expanded in the crate being compiled, which here is the
/// trybuild package rather than `protocol-droid`. That is the whole point:
/// the value below is this crate's version, not the dependency's.
const THIS_CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[protocol_droid::server(name = "implicit")]
impl Implicit {
    /// A tool, so the server is not empty.
    #[protocol_droid::tool]
    async fn ping() -> Result<String, String> {
        Ok("pong".to_string())
    }
}

#[protocol_droid::server(name = "explicit", version = env!("CARGO_PKG_VERSION"))]
impl Explicit {
    /// A tool, so the server is not empty.
    #[protocol_droid::tool]
    async fn ping() -> Result<String, String> {
        Ok("pong".to_string())
    }
}

#[protocol_droid::server(name = "literal", version = "9.9.9")]
impl Literal {
    /// A tool, so the server is not empty.
    #[protocol_droid::tool]
    async fn ping() -> Result<String, String> {
        Ok("pong".to_string())
    }
}

/// Reports a mismatch as a panic, which fails the trybuild case.
///
/// A `panic!` here is not a test failure the way it would be in a `#[test]`:
/// trybuild runs the binary and treats a non-zero exit as a failed case,
/// and the message is printed for whoever is reading the CI log. The
/// alternative — a `Result` returned from `main` — would be swallowed by
/// `Termination`'s `Debug` bound.
fn expect_version(what: &str, got: &str, wanted: &str) {
    assert_eq!(
        got, wanted,
        "the {what} server reports {got}, expected {wanted}"
    );
}

fn main() {
    let implicit = Implicit.into_server().expect("the implicit server builds");
    expect_version("implicit", implicit.version(), THIS_CRATE_VERSION);

    let explicit = Explicit.into_server().expect("the explicit server builds");
    expect_version("explicit", explicit.version(), THIS_CRATE_VERSION);

    let literal = Literal.into_server().expect("the literal server builds");
    expect_version("literal", literal.version(), "9.9.9");
}
