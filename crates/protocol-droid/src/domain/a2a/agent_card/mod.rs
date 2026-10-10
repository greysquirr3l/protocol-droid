//! The A2A `AgentCard` — the self-describing manifest an agent publishes
//! at `/.well-known/agent-card.json`.
//!
//! # What this is
//!
//! A `AgentCard` is a JSON document an A2A server publishes to advertise
//! itself: its name, its capabilities, the protocol bindings it supports
//! (`JSONRPC`, `GRPC`, `HTTP+JSON`), the URLs those bindings are
//! reachable at, and the skills it offers. A client fetches a card
//! before sending its first request, and uses it to decide which
//! methods to call, which auth scheme to use, and which interface URL
//! to dial.
//!
//! Source: A2A v1.0 specification, §4.4 "Agent Card" and §8 "Agent
//! Discovery". The proto at `crates/protocol-droid-schema/spec/a2a/v1.0/
//! a2a.proto` is the normative source; this value object is the Rust
//! shape the wire codec lowers into.
//!
//! # Why three files
//!
//! The proto has fifteen fields. Splitting the value objects, the
//! error type, and the builder across [`types`], [`error`], and
//! [`builder`] keeps each file focused on a single concern:
//!
//! - [`types`] holds the wire-shape value objects and the
//!   `LazyLock` constants for the three spec-named protocol
//!   bindings.
//! - [`error`] holds the typed `AgentCardError` enum — a single
//!   enum in its own file so a `match` is greppable and a
//!   new variant does not touch the value-object file.
//! - [`builder`] holds `AgentCardBuilder`, the only way to get an
//!   `AgentCard` once the optional fields land.
//!
//! # Required fields, by spec
//!
//! The proto marks eight fields as REQUIRED. The builder enforces
//! them at construction: `name` non-empty, `description` non-empty,
//! `version` non-empty, `supported_interfaces` non-empty (an agent
//! with no interface has no protocol to speak),
//! `default_input_modes` non-empty, `default_output_modes` non-empty,
//! `skills` (a required field, may be an empty list — an agent with
//! no skills is a valid agent that simply refuses every request),
//! and `capabilities` (always set, the spec treats "no
//! capabilities" as a value the client reads).

mod builder;
mod error;
mod oauth_flow;
mod security_requirement;
mod security_scheme;
mod types;

pub use builder::AgentCardBuilder;
pub use error::AgentCardError;
pub use oauth_flow::{
    AuthorizationCodeOAuthFlow, ClientCredentialsOAuthFlow, DeviceCodeOAuthFlow,
    OAuth2SecurityScheme, OAuthFlows,
};
pub use security_requirement::{SecurityRequirement, StringList};
pub use security_scheme::{
    APIKeyLocation, APIKeySecurityScheme, HTTPAuthSecurityScheme, MutualTlsSecurityScheme,
    OpenIdConnectSecurityScheme, SecurityScheme,
};
pub use types::{
    AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentProvider, AgentSkill,
    ProtocolBinding, GRPC, HTTP_JSON, JSONRPC,
};
