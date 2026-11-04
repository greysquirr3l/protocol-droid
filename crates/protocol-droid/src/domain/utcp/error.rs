//! The errors a UTCP manual can fail with.
//!
//! # Why a manual's errors are their own type
//!
//! A manual is a document, not a message on a wire, so the failures are
//! document failures: the JSON does not parse, a required field is absent, a
//! version is not a version. None of those are protocol errors in the sense
//! the MCP and A2A modules use — no peer is waiting on a reply, and there is
//! no error code to send anybody. Naming them separately keeps the reader of
//! a failure from hunting for a JSON-RPC error code in a document that never
//! had one.

use crate::domain::utcp::version::VersionError;

/// A manual that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManualError {
    /// The text is not JSON.
    #[error("the manual is not JSON: {detail}")]
    InvalidJson {
        /// What the parser said.
        detail: String,
    },
    /// The JSON is valid but is not the shape the manual needs.
    #[error("the {what} is not a JSON object")]
    NotAnObject {
        /// Which part of the document was wrong.
        what: &'static str,
    },
    /// A required field is absent.
    #[error("the {what} has no `{field}`")]
    MissingField {
        /// Which part of the document was wrong.
        what: &'static str,
        /// The field that was absent.
        field: &'static str,
    },
    /// A field is present but is not what it has to be.
    #[error("the {what} has a `{field}` that is not usable: {detail}")]
    InvalidField {
        /// Which part of the document was wrong.
        what: &'static str,
        /// The field that was wrong.
        field: &'static str,
        /// What was wrong with it.
        detail: String,
    },
    /// A version field is not a version.
    #[error("the {field} of the {what} is not a version: {source}")]
    InvalidVersion {
        /// Which part of the document was wrong.
        what: &'static str,
        /// The field that was wrong.
        field: &'static str,
        /// The string that was not a version.
        raw: String,
        /// Why it was not one.
        #[source]
        source: VersionError,
    },
}
