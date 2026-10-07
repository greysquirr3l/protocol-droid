//! The `protocol-droid` command-line adapter, as a library.
//!
//! ## Why there is a library target at all
//!
//! The binary is a thin `main` that parses arguments and dispatches. Everything
//! it can actually do lives here, and that split is not cosmetic:
//!
//! `unreachable_pub` and `redundant_pub_crate` are both deny-level in this
//! project, and in a crate that is *only* a binary they contradict each other —
//! a `pub` item in a bin crate is genuinely unreachable from outside, and a
//! `pub(crate)` one is genuinely redundant. There is no spelling that satisfies
//! both. In a crate that also has a library target, `pub` means what it says
//! and the question does not arise.
//!
//! So `main.rs` holds the argument contract and the dispatch, and this crate
//! holds the behaviour. The alternative — merging the subcommand into
//! `main.rs` to dodge the lint — would trade a real lint conflict for a
//! 500-line file the project's own file-structure rules disallow.

pub mod bridge;
pub mod check;
