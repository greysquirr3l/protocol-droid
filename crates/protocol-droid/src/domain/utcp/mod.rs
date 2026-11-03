//! UTCP domain — the manual, the tool, the call template, and the version.
//!
//! # What UTCP is, in one paragraph
//!
//! UTCP is not a message protocol. It is a *manual*: a JSON document that
//! lists the tools a provider offers and says how to call each one in the
//! tool's own protocol. A client fetches the manual at `GET /utcp`, reads
//! the call template, and then makes the call itself — an HTTP request, a
//! command line, an MCP `tools/call` — against the endpoint the manual
//! names. There is no session, no negotiation, and no request the provider
//! has to answer.
//!
//! # Where this sits in the crate
//!
//! The crate's centre is a canonical IR with per-revision codecs on either
//! side, and the cross-protocol bridge composes two codecs into a
//! translation between two dialects. UTCP has no dialect, so it does not
//! enter through that seam: there is nothing to lift from and nothing to
//! lower into. What it has is a *document* and a *call*, and this module is
//! the value objects for both.
//!
//! The three directions the crate supports with it:
//!
//! - **Read a manual.** [`Manual::parse`] turns a provider's document into
//!   typed values.
//! - **Publish a manual.** The application layer builds a [`Manual`] from
//!   this crate's own tools, and the primary adapter serves it at
//!   `GET /utcp`.
//! - **Call a tool.** The application layer turns a tool and its arguments
//!   into a concrete call, and the secondary adapters make it.
//!
//! # What is modelled exactly, and what is passed through
//!
//! The call template types the specification spells out field by field —
//! `http`, `cli`, `mcp` and `file` — are modelled field by field. A type it
//! names but does not describe is kept verbatim in [`Transport::Other`], and
//! an authentication block of an unrecognised kind in [`Auth::Other`]. A
//! manual that describes something this build has never heard of is still a
//! readable manual, and a reader that dropped what it did not understand
//! would be worse than one that said so.

pub mod auth;
pub mod call_template;
pub mod endpoint;
pub mod error;
pub mod manual;
pub mod parse;
pub mod policy;
#[cfg(test)]
mod tests;
pub mod variables;
pub mod version;

pub use auth::{ApiKeyLocation, Auth};
pub use call_template::{
    CallTemplate, CliCommand, CliTransport, FileTransport, HttpMethod, HttpTransport, McpConfig,
    McpServer, McpTransport, OtherTransport, Transport,
};
pub use endpoint::{Endpoint, EndpointError};
pub use error::ManualError;
pub use manual::{Manual, Tool};
pub use policy::ProtocolPolicy;
pub use version::{UtcpVersion, VersionError};
