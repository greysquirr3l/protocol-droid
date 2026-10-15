//! `initialize` — the editor's first call, the agent's first reply.
//!
//! Source of truth:
//! `crates/protocol-droid-schema/spec/acp/v1.25.0/schema.json`
//! under `$defs.InitializeRequest` and `$defs.InitializeResponse`.
//!
//! # The minimum-viable initialize
//!
//! A complete wire round-trip needs:
//! - the `protocolVersion` integer on both sides
//! - the `clientCapabilities` and `agentCapabilities` blocks
//! - the optional `clientInfo` / `agentInfo` block
//! - the optional `authMethods` on the response (gated on
//!   the client advertising `auth.terminal = true`)
//!
//! The capability shapes are nested (`fs`, `terminal`,
//! `promptCapabilities`, `mcpCapabilities`, etc.). A
//! consumer that does not need the nested surface can
//! leave them at the schema's defaults and the
//! round-trip is still well-formed.
//!
//! # Why a hand-rolled `ClientCapabilities` is OK here
//!
//! The full set of capability types the schema names
//! (`FileSystemCapabilities`, `AuthCapabilities`,
//! `PromptCapabilities`, `McpCapabilities`,
//! `SessionCapabilities`, `ClientSessionCapabilities`,
//! `ElicitationCapabilities`, ...) is the bulk of the
//! 178-definition schema. Building every one of them
//! is its own commit. This module ships the minimum
//! the `initialize` round-trip needs: a `ClientCapabilities`
//! and `AgentCapabilities` that pass through the schema's
//! default values for the un-built sub-types. A future
//! increment fills in the per-capability shapes.

use serde::{Deserialize, Serialize};

use super::implementation::Implementation;
use super::protocol_version::AcpProtocolVersion;

/// The `initialize` request body. Sent by the editor to
/// the agent as its first call.
///
/// The schema's `clientCapabilities` field has a default
/// object (the `x-deserialize-default-on-error` annotation),
/// so a request with `clientCapabilities` absent on the
/// wire is well-formed: the default is the all-off
/// capabilities block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    /// The latest protocol version the client supports.
    /// The current pin is `AcpProtocolVersion::CURRENT`.
    pub protocol_version: AcpProtocolVersion,
    /// The client's capabilities. The schema defaults
    /// this to the all-off object; the typed default
    /// is the same.
    #[serde(default)]
    pub client_capabilities: ClientCapabilities,
    /// Information about the client (name, version).
    /// The schema marks this "in future versions of the
    /// protocol, this will be required" — today it is
    /// optional but well-behaved clients always send it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_info: Option<Implementation>,
}

/// The `initialize` response body. Sent by the agent in
/// reply to `InitializeRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    /// The protocol version the agent is speaking.
    /// The negotiation rule is "the lower of the two";
    /// the agent's value is the one the editor takes.
    pub protocol_version: AcpProtocolVersion,
    /// The agent's capabilities. Same defaulting
    /// behaviour as the request's `clientCapabilities`.
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    /// The auth methods the agent supports. Optional
    /// in the schema; absent means "no auth, anonymous
    /// connection". Present only when the client
    /// advertised `auth.terminal = true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_methods: Option<Vec<AuthMethod>>,
    /// Information about the agent (name, version).
    /// Optional; well-behaved agents always send it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<Implementation>,
}

/// The editor's capabilities block. Source of truth:
/// `schema.json::$defs.ClientCapabilities`.
///
/// The shape is a flat bag of capability sub-types; each
/// sub-type is itself a typed newtype. A consumer that
/// only needs the wire form sends a `ClientCapabilities`
/// with the relevant fields; the rest take the schema's
/// defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// File-system access. The schema's default is
    /// `{ "readTextFile": false, "writeTextFile": false }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs: Option<FileSystemCapabilities>,
    /// Whether the client supports every `terminal/*`
    /// method. The schema's default is `false`. The
    /// `skip_serializing_if` keeps the wire form
    /// minimal for the all-off case.
    #[serde(default, skip_serializing_if = "is_false")]
    pub terminal: bool,
    /// Whether the client supports `auth` methods.
    /// The schema's default is `{ "terminal": false }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthCapabilities>,
}

/// The `is_false` predicate for `#[serde(skip_serializing_if)]`.
/// `serde` passes the field by reference even when the
/// field is a `Copy` primitive; the helper has to take
/// `&bool` for the macro to type-check. The body is a
/// single deref-and-negate.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if takes &T regardless of whether T is Copy"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// The agent's capabilities block. Source of truth:
/// `schema.json::$defs.AgentCapabilities`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// Whether the agent supports `session/load`.
    /// The schema's default is `false`. The
    /// `skip_serializing_if` keeps the wire form
    /// minimal for the all-off case.
    #[serde(default, skip_serializing_if = "is_false")]
    pub load_session: bool,
    /// The prompt capabilities the agent supports
    /// (image, audio, embedded context). The
    /// schema's default is the all-off object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_capabilities: Option<PromptCapabilities>,
    /// The MCP capabilities the agent supports (http,
    /// sse). The schema's default is the all-off object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_capabilities: Option<McpCapabilities>,
}

/// The file-system capabilities the client advertises.
///
/// The editor turns on `read_text_file` / `write_text_file`
/// when the agent may call the corresponding
/// `fs/read_text_file` / `fs/write_text_file` methods.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSystemCapabilities {
    /// Whether the client accepts
    /// `fs/read_text_file` calls.
    #[serde(default)]
    pub read_text_file: bool,
    /// Whether the client accepts
    /// `fs/write_text_file` calls.
    #[serde(default)]
    pub write_text_file: bool,
}

/// The auth capabilities the client advertises. The
/// only field the v1.25.0 schema names is `terminal`
/// (whether the client can present a terminal-based
/// auth flow).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthCapabilities {
    /// Whether the client supports terminal-based
    /// auth flows.
    #[serde(default)]
    pub terminal: bool,
}

/// The prompt capabilities the agent advertises.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    /// Whether the agent accepts image content in
    /// user prompts.
    #[serde(default)]
    pub image: bool,
    /// Whether the agent accepts audio content in
    /// user prompts.
    #[serde(default)]
    pub audio: bool,
    /// Whether the agent accepts embedded context
    /// (e.g. file contents) in user prompts.
    #[serde(default)]
    pub embedded_context: bool,
}

/// The MCP capabilities the agent advertises. The
/// agent is itself an MCP client; these flags say
/// which transports the agent speaks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCapabilities {
    /// Whether the agent accepts MCP `http` servers.
    #[serde(default)]
    pub http: bool,
    /// Whether the agent accepts MCP `sse` servers.
    #[serde(default)]
    pub sse: bool,
}

/// The auth method the agent advertises. The schema
/// is a discriminated union (`AuthMethodAgent` /
/// `AuthMethodTerminal`); the typed form is the
/// same `enum` shape.
///
/// The schema's `$defs.AuthMethod` definition is
/// exhaustive over the two variants; a `null`
/// payload is not a valid `AuthMethod`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum AuthMethod {
    /// An agent-to-agent auth method.
    #[serde(rename = "agent")]
    Agent(AuthMethodAgent),
    /// A terminal-based auth method.
    #[serde(rename = "terminal")]
    Terminal(AuthMethodTerminal),
}

/// The `agent` variant of [`AuthMethod`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethodAgent {
    /// The auth method id. A string the agent and
    /// editor both recognise (e.g. an OAuth client
    /// id).
    pub id: String,
    /// A human-readable display name.
    pub name: String,
    /// A human-readable description.
    pub description: String,
}

/// The `terminal` variant of [`AuthMethod`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethodTerminal {
    /// A unique identifier for this auth method.
    /// The schema names every `AuthMethod`
    /// carries an `id`; the value is opaque to
    /// the wire and meaningful only to the
    /// agent and editor that negotiated it.
    pub id: String,
    /// A human-readable display name.
    pub name: String,
    /// A human-readable description.
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The test error type. `serde_json` calls are
    /// infallible for a well-formed value, but the
    /// test functions return `Result` to satisfy
    /// the project's no-`expect` rule.
    #[derive(Debug)]
    #[expect(
        dead_code,
        reason = "the test only constructs the error variant on failure; the field is read by the Debug derive"
    )]
    enum Failure {
        Json(serde_json::Error),
        /// A fallible check on the deserialised
        /// value (an absent field, an empty
        /// list, etc.). The string is the
        /// human-readable reason.
        Other(String),
    }

    impl From<serde_json::Error> for Failure {
        fn from(error: serde_json::Error) -> Self {
            Self::Json(error)
        }
    }

    /// A minimal `InitializeRequest` serialises
    /// to the wire form the schema names. The
    /// `clientCapabilities` field is `{}` on the
    /// wire (the schema's `x-deserialize-default-on-error`
    /// default is the all-off object, but a
    /// `Default` typed value is the empty
    /// object literal).
    #[test]
    fn a_minimal_request_serialises_to_the_wire_form() -> Result<(), Failure> {
        let request = InitializeRequest {
            protocol_version: AcpProtocolVersion::CURRENT,
            client_capabilities: ClientCapabilities::default(),
            client_info: None,
        };
        let value = serde_json::to_value(&request)?;
        let protocol_version = value
            .get("protocolVersion")
            .and_then(serde_json::Value::as_u64);
        let client_capabilities = value.get("clientCapabilities");
        assert_eq!(protocol_version, Some(1));
        assert_eq!(client_capabilities, Some(&json!({})));
        assert!(value.get("clientInfo").is_none(), "None omits the field");
        Ok(())
    }

    /// A full `InitializeRequest` with every
    /// capability turned on serialises to the
    /// expected wire form. The round-trip
    /// through `Serialize` → `Deserialize`
    /// produces an equal value.
    #[test]
    fn a_full_request_round_trips() -> Result<(), Failure> {
        let request = InitializeRequest {
            protocol_version: AcpProtocolVersion::CURRENT,
            client_capabilities: ClientCapabilities {
                fs: Some(FileSystemCapabilities {
                    read_text_file: true,
                    write_text_file: true,
                }),
                terminal: true,
                auth: Some(AuthCapabilities { terminal: true }),
            },
            client_info: Some(Implementation::protocol_droid("test", "0.1.0")),
        };
        let json = serde_json::to_string(&request)?;
        let round_tripped: InitializeRequest = serde_json::from_str(&json)?;
        assert_eq!(round_tripped, request);
        Ok(())
    }

    /// The `AuthMethod` discriminator is `type`
    /// on the wire, with the two variants
    /// `agent` and `terminal`. A `null` is not
    /// a valid `AuthMethod`; the round-trip is
    /// exhaustive over the two variants.
    #[test]
    fn auth_method_discriminator_is_the_type_field() -> Result<(), Failure> {
        let agent = AuthMethod::Agent(AuthMethodAgent {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            description: "An auth method".to_owned(),
        });
        let terminal = AuthMethod::Terminal(AuthMethodTerminal {
            id: "dev-terminal".to_owned(),
            name: "Terminal".to_owned(),
            description: "A terminal auth method".to_owned(),
        });
        let agent_value = serde_json::to_value(&agent)?;
        let terminal_value = serde_json::to_value(&terminal)?;
        let agent_type = agent_value.get("type").and_then(serde_json::Value::as_str);
        let terminal_type = terminal_value
            .get("type")
            .and_then(serde_json::Value::as_str);
        assert_eq!(agent_type, Some("agent"));
        assert_eq!(terminal_type, Some("terminal"));
        Ok(())
    }

    /// A minimal `InitializeResponse` round-trips
    /// through `Serialize` → `Deserialize`. The
    /// `agentCapabilities` defaults to the all-off
    /// block; the `authMethods` is omitted when
    /// `None`; the `agentInfo` is the protocol-droid
    /// `Implementation`.
    #[test]
    fn a_minimal_response_round_trips() -> Result<(), Failure> {
        let response = InitializeResponse {
            protocol_version: AcpProtocolVersion::CURRENT,
            agent_capabilities: AgentCapabilities {
                load_session: true,
                prompt_capabilities: Some(PromptCapabilities {
                    image: true,
                    audio: false,
                    embedded_context: true,
                }),
                mcp_capabilities: Some(McpCapabilities {
                    http: true,
                    sse: false,
                }),
            },
            auth_methods: None,
            agent_info: Some(Implementation::protocol_droid("test-agent", "0.1.0")),
        };
        let json = serde_json::to_string(&response)?;
        let round_tripped: InitializeResponse = serde_json::from_str(&json)?;
        assert_eq!(round_tripped, response);
        Ok(())
    }

    /// A response with an `authMethods` list
    /// serialises the list on the wire. A `null`
    /// or absent `authMethods` means the agent
    /// does not require auth, which is the
    /// default for an anonymous connection.
    #[test]
    fn a_response_with_auth_methods_carries_them_on_the_wire() -> Result<(), Failure> {
        let response = InitializeResponse {
            protocol_version: AcpProtocolVersion::CURRENT,
            agent_capabilities: AgentCapabilities::default(),
            auth_methods: Some(vec![AuthMethod::Terminal(AuthMethodTerminal {
                id: "dev-terminal".to_owned(),
                name: "Terminal".to_owned(),
                description: "Sign in via the terminal".to_owned(),
            })]),
            agent_info: None,
        };
        let value = serde_json::to_value(&response)?;
        let methods = value
            .get("authMethods")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Failure::Other("authMethods is not an array".to_owned()))?;
        let first = methods
            .first()
            .ok_or_else(|| Failure::Other("authMethods is empty".to_owned()))?;
        let first_type = first.get("type").and_then(serde_json::Value::as_str);
        assert_eq!(first_type, Some("terminal"));
        Ok(())
    }
}
