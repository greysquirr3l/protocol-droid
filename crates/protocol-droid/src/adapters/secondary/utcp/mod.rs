//! The UTCP client side: reading a manual, and making the call it describes.
//!
//! # What lives here
//!
//! - [`manual`] fetches a provider's manual over HTTP.
//! - [`call()`] makes the call a manual's tool describes, over HTTP or over an
//!   MCP session.
//!
//! Both are *driven* adapters: they talk to a provider this build does not
//! control. The decisions they make - which URL, which headers, which
//! credential - are made in the application layer
//! (`application::utcp::call`), and the adapter only carries them out.

pub mod call;
pub mod error;
pub mod manual;
pub mod token_cache;

pub use call::{CallOutcome, call, call_http, call_mcp};
pub use error::UtcpError;
pub use manual::ManualClient;
pub use token_cache::{TokenCache, TokenKey};
