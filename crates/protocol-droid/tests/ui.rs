//! What the attribute macros refuse, and what they accept.
//!
//! `tests/macros.rs` calls every tool and reads every listing, which is the
//! only way to catch an expansion that typechecks and is still wrong. It
//! cannot catch the other half: a shape the macro never intended to accept
//! that happens to compile. The cases here are that half.
//!
//! Each file under `tests/ui/` is its own crate, so a case is a whole program
//! rather than a fragment. `pass/` is the API's real surface, written the way
//! the macro documents it, and a failure in one is a defect in the expansion.
//! `fail/` is the set of mistakes a user actually makes, and each `.stderr`
//! next to it is the message they will read.
//!
//! # Why the `.stderr` files are worth reading rather than pinning blindly
//!
//! A pinned expectation is a promise. `error: expected one of #, {` is a
//! promise that says nothing about what to do next, and it is worse than no
//! test at all, because the next regression is then hidden behind it. Every
//! message under `fail/` names the half of the signature to change; if one
//! stops doing so, the macro is at fault and the fix belongs in the macro
//! rather than in the pinned text.
//!
//! The text is not stable across toolchains, and that is the intended cost. A
//! `.stderr` that only passes because of an `#[expect]` or a version gate is
//! not testing the message. Regenerate with `TRYBUILD=overwrite` and read the
//! diff.

/// Compiles every case, then compares each `fail/` run against its `.stderr`.
#[test]
fn macros_accept_and_refuse() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/pass/*.rs");
    cases.compile_fail("tests/ui/fail/*.rs");
}
