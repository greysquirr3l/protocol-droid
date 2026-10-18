//! Protocol-specific application logic shared by more than one entry point.
//!
//! These modules are MCP-shaped rather than transport-shaped, and more than
//! one caller needs them: a server answers a handshake, a bridge answers the
//! same handshake on its downstream leg. Anything here that both needed would
//! have been written twice otherwise, and two readings of a wire format is how
//! a bridge ends up disagreeing with the server it sits in front of.
//!
//! Post-MVP protocols slot in beside `initialize` in this directory.

pub mod initialize;
pub mod input_required;
#[cfg(feature = "http")]
pub mod param_headers;
pub mod request_meta;
