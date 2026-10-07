//! The Streamable HTTP client, and nothing else.
//!
//! The mirror image of [`crate::adapters::primary::transport::http`]. The
//! server half decides who is calling, how large a body is, and which session;
//! this half does the same three things from the other side, plus the one the
//! server does not need: surviving a dropped stream without repeating or
//! skipping an event.
//!
//! Neither half names an MCP method or a revision. That is the property that
//! makes the transport reusable by the version-translating bridge, which has
//! to carry revisions it does not understand and therefore cannot have a
//! method table baked into its socket layer. The one MCP-shaped name here is
//! the session header, and it is named in exactly one constant so a revision
//! that spells it differently is a one-line change rather than a search.
//!
//! Three files, one concern each: [`client`] holds the request/response
//! conversation and the resume state machine, [`frames`] turns bytes into
//! frames, and [`error`] is the one enum a caller above this layer matches on.
//! The frame *grammar* is not re-implemented here — it is shared with the
//! server's codec, because a client with its own grammar is a client that
//! disagrees with the server about where an event ends.
//!
//! # Examples
//!
//! A client that has been handed a session id, sending a body and reading a
//! document back:
//!
//! ```
//! use protocol_droid::adapters::secondary::transport::http::{
//!     HttpClient, HttpClientConfig,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
//! # let endpoint = "https://example.invalid/mcp";
//! let client = HttpClient::new(HttpClientConfig::new(endpoint)?)?;
//!
//! // No session yet, so this call cannot require one. A peer that answers
//! // with a status the transport does not recognise is a `Refused`, and
//! // nothing here has to look at the status to tell that from a dead socket.
//! assert!(matches!(client.session_id(), None));
//! # Ok::<_, Box<dyn std::error::Error>>(())
//! # })
//! # }
//! ```

mod bridge_peer;
mod client;
mod error;
mod frames;

#[cfg(test)]
mod tests;

pub use client::{
    HttpClient, HttpClientConfig, HttpReply, HttpResponse, HttpStream, Resumable, ResumableStream,
    RetryBudget,
};
pub use error::HttpClientError;
pub use frames::{FrameError, FrameReader, split_block};
