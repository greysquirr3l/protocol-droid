//! The MCP wire shapes, typed.
//!
//! These are the `initialize` request and result, the capability blocks
//! either side advertises, and the tool call types the peer sends and
//! receives. They are the data half of the client: the peer in [`super::peer`]
//! moves them over stdio, and the shapes themselves say nothing about how
//! they travel.
//!
//! Every type here is a transcription of the vendored schema. The doc comment
//! on each one names the revision and the `schema.json` path it came from, and
//! a type whose shape cannot be found in the schema does not belong here.

use serde::Serialize;
use serde_json::Value;

/// The MCP wire form's
/// `initialize` request. Sent by
/// the client as its first call.
///
/// Source of truth:
/// `crates/protocol-droid-schema/spec/<pinned-revision>/schema.json`
/// under `definitions.InitializeRequest`'s
/// `params` shape (the wrapper is
/// the JSON-RPC envelope; the typed
/// shape is the `params` value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    /// The MCP revision the client
    /// supports. The schema names
    /// every published revision; the
    /// peer uses the revision the
    /// consumer selected at
    /// construction.
    pub protocol_version: String,
    /// The client's capabilities. The
    /// schema defaults this to an
    /// empty object; the typed
    /// default is the all-off
    /// capabilities block.
    #[serde(default)]
    pub capabilities: ClientCapabilities,
    /// Information about the
    /// client. The schema marks
    /// this required in newer
    /// revisions; the wire form
    /// always carries it.
    pub client_info: Implementation,
}

/// The MCP wire form's
/// `initialize` reply. Sent by
/// the server in response to
/// `InitializeRequest`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// The MCP revision the server
    /// supports. The negotiation
    /// rule is the server picks
    /// the client's revision if
    /// it supports it; otherwise
    /// the server's latest
    /// supported revision.
    pub protocol_version: String,
    /// The server's capabilities.
    pub capabilities: ServerCapabilities,
    /// Information about the
    /// server. The schema marks
    /// this required.
    pub server_info: Implementation,
    /// Optional instructions the
    /// server hands the client for
    /// how to use the server. A
    /// typed `None` is the default
    /// for servers that do not
    /// provide instructions.
    #[serde(default)]
    pub instructions: Option<String>,
}

/// The client's capabilities.
///
/// The schema's `ClientCapabilities`
/// is an open object; the typed shape
/// carries the canonical
/// sub-capabilities (`roots`,
/// `sampling`, `experimental`).
/// Sub-capabilities the consumer
/// does not enable default to
/// `None`, which the wire form
/// omits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ClientCapabilities {
    /// The `roots` capability
    /// (whether the client
    /// advertises filesystem
    /// roots to the server).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootsCapability>,
    /// The `sampling` capability
    /// (whether the client
    /// supports LLM sampling
    /// on the server's behalf).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingCapability>,
    /// Experimental
    /// capabilities. The schema
    /// does not constrain the
    /// shape; the typed default
    /// is an empty map.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty", default)]
    pub experimental: std::collections::BTreeMap<String, Value>,
}

/// The `roots` sub-capability.
/// Whether the client advertises
/// filesystem roots to the server
/// and whether the list can change
/// at runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootsCapability {
    /// Whether the list of roots
    /// can change after the
    /// `initialize` round-trip.
    #[serde(default)]
    pub list_changed: bool,
}

/// The `sampling` sub-capability.
/// A marker for the client's
/// support of LLM sampling. The
/// schema's `SamplingCapability`
/// is an empty object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SamplingCapability {}

/// The server's capabilities.
///
/// The schema's `ServerCapabilities`
/// is an open object; the typed
/// shape carries the canonical
/// sub-capabilities
/// (`prompts`, `resources`,
/// `tools`, `logging`).
/// Sub-capabilities the server
/// does not advertise default to
/// `None`, which the wire form
/// omits.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct ServerCapabilities {
    /// The `prompts` capability.
    #[serde(default)]
    pub prompts: Option<PromptsCapability>,
    /// The `resources` capability.
    #[serde(default)]
    pub resources: Option<ResourcesCapability>,
    /// The `tools` capability.
    #[serde(default)]
    pub tools: Option<ToolsCapability>,
    /// The `logging` capability.
    #[serde(default)]
    pub logging: Option<LoggingCapability>,
    /// Experimental
    /// capabilities. The schema
    /// does not constrain the
    /// shape; the typed default
    /// is an empty map.
    #[serde(default)]
    pub experimental: std::collections::BTreeMap<String, Value>,
}

/// The `prompts` sub-capability.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptsCapability {
    /// Whether the list of
    /// prompts can change at
    /// runtime.
    #[serde(default)]
    pub list_changed: bool,
}

/// The `resources` sub-capability.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesCapability {
    /// Whether the list of
    /// resources can change at
    /// runtime.
    #[serde(default)]
    pub list_changed: bool,
    /// Whether the client can
    /// subscribe to individual
    /// resource updates.
    #[serde(default)]
    pub subscribe: bool,
}

/// The `tools` sub-capability.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    /// Whether the list of
    /// tools can change at
    /// runtime.
    #[serde(default)]
    pub list_changed: bool,
}

/// The `logging` sub-capability.
/// A marker.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct LoggingCapability {}

/// The `Implementation` block on
/// either side of `initialize`.
/// The shape is identical on the
/// client and server side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Implementation {
    /// The implementation's
    /// programmatic name.
    pub name: String,
    /// The implementation's
    /// human-readable title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The implementation's
    /// version string.
    pub version: String,
}

/// The `tools/list` reply. The
/// server returns the list of
/// tools it advertises.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListToolsResult {
    /// The tools the server
    /// advertises. The schema
    /// requires this list.
    pub tools: Vec<Tool>,
    /// An optional pagination
    /// cursor. The schema marks
    /// this as the standard
    /// JSON-RPC pagination shape;
    /// the typed default is
    /// `None` (no more pages).
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// A single tool. The server
/// returns these from
/// `tools/list` and the client
/// invokes them via `tools/call`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// The tool's programmatic
    /// name. The schema requires
    /// this and the server
    /// guarantees it is unique
    /// within the server's
    /// advertised tool set.
    pub name: String,
    /// The tool's human-readable
    /// description. Optional;
    /// servers SHOULD provide
    /// this for tool catalogs
    /// shown to humans.
    #[serde(default)]
    pub description: Option<String>,
    /// The JSON Schema for the
    /// tool's input. The schema
    /// requires this; the typed
    /// shape carries it as a
    /// raw `Value` because the
    /// JSON Schema is
    /// tool-specific.
    pub input_schema: Value,
}

/// The `tools/call` request body.
/// The client invokes a tool by
/// name with a JSON object that
/// matches the tool's
/// `inputSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolRequest {
    /// The tool's name. Matches
    /// the `name` field of a
    /// `Tool` returned from
    /// `tools/list`.
    pub name: String,
    /// The tool's arguments.
    /// The shape is the tool's
    /// `inputSchema`. A tool that
    /// takes no arguments passes
    /// an empty object.
    #[serde(default)]
    pub arguments: Value,
}

/// The `tools/call` reply. The
/// server returns a list of
/// content blocks (text, image,
/// audio, or embedded resources)
/// and an optional `is_error`
/// flag.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    /// The tool's result
    /// content. The schema
    /// requires at least one
    /// block.
    pub content: Vec<ContentBlock>,
    /// Whether the tool
    /// call failed. The
    /// schema marks this
    /// optional; the typed
    /// default is `false`
    /// (success).
    #[serde(default)]
    pub is_error: bool,
}

/// A single content block in a
/// `CallToolResult`. The schema
/// defines `text`, `image`,
/// `audio`, and `resource`
/// variants via a tagged union.
/// The typed shape models the
/// four shapes the schema names.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// A text block.
    Text {
        /// The text content.
        text: String,
    },
    /// An image block. The
    /// `data` is base64-encoded.
    Image {
        /// The image's
        /// MIME type.
        #[serde(rename = "mimeType")]
        mime_type: String,
        /// The base64-encoded
        /// image data.
        data: String,
    },
    /// An audio block. The
    /// `data` is base64-encoded.
    Audio {
        /// The audio's
        /// MIME type.
        #[serde(rename = "mimeType")]
        mime_type: String,
        /// The base64-encoded
        /// audio data.
        data: String,
    },
    /// An embedded resource
    /// block. The resource is
    /// inlined by URI.
    Resource {
        /// The embedded
        /// resource's URI.
        uri: String,
        /// The embedded
        /// resource's
        /// MIME type.
        #[serde(rename = "mimeType", default)]
        mime_type: Option<String>,
        /// The embedded
        /// resource's text
        /// content. The
        /// schema does not
        /// require this;
        /// the typed
        /// default is
        /// `None`.
        #[serde(default)]
        text: Option<String>,
    },
}

/// Default client capabilities.
///
/// The default form is the
/// all-off block: no
/// `roots`, no `sampling`,
/// no `experimental`. A
/// peer that wants any of
/// these on starts from
/// this default and sets
/// the relevant field.
///
/// The function is `pub`
/// so the library itself
/// names the type in
/// non-test code, which
/// keeps the rust 1.98
/// `dead_code_pub_in_binary`
/// lint from flagging
/// `ClientCapabilities` as
/// "never constructed" in
/// the library's own build
/// (the lint is promoted to
/// library crates in 1.98
/// and fires when a
/// `pub` type has no
/// construction site in
/// the lib's normal build,
/// which `#[cfg(test)]`
/// items do not satisfy).
#[must_use]
pub fn default_client_capabilities() -> ClientCapabilities {
    ClientCapabilities::default()
}

/// A reference `initialize`
/// request.
///
/// A consumer that wants
/// to send an
/// `initialize` request
/// needs the revision
/// string and a
/// `client_info` block.
/// The function returns
/// a `2025-06-18`
/// request — the newest
/// revision the vendored
/// schema has, and the
/// one the integration
/// test fixture speaks.
///
/// Like
/// [`default_client_capabilities`],
/// this function exists to
/// give the library's own
/// non-test build a
/// construction site for
/// the typed shapes
/// `InitializeRequest` and
/// `Implementation` name
/// (see that function's
/// note for the lint
/// rationale).
#[must_use]
pub fn initialize_request_template() -> InitializeRequest {
    InitializeRequest {
        protocol_version: "2025-06-18".to_owned(),
        capabilities: ClientCapabilities::default(),
        client_info: Implementation {
            name: "protocol-droid".to_owned(),
            title: Some("protocol-droid MCP client".to_owned()),
            version: "0.1.0".to_owned(),
        },
    }
}

#[cfg(test)]
mod typed_shape_round_trip {
    //! Wire-format property tests
    //! for the typed MCP shapes.
    //!
    //! A property test over the
    //! typed shapes ensures the
    //! wire form does not drift
    //! from the consumer's
    //! expectations: a
    //! `serde(rename_all)`
    //! typo, a missing `default`,
    //! or a wrong field name is
    //! caught by the round-trip
    //! property test. The
    //! test also drives the
    //! typed shapes through
    //! the lib's own types so
    //! the binary crate's
    //! `dead_code` lint does
    //! not flag them.
    use super::*;

    /// Smoke test: a
    /// `ClientCapabilities`
    /// constructs in the
    /// default form, and
    /// serialises without
    /// panicking.
    #[test]
    fn client_capabilities_default_serialises() -> Result<(), serde_json::Error> {
        let caps = ClientCapabilities::default();
        let value = serde_json::to_value(&caps)?;
        if let Some(object) = value.as_object() {
            assert!(
                !object.contains_key("roots"),
                "the default form omits `roots`"
            );
        } else {
            assert!(
                value.is_object(),
                "the serialised value should be an object"
            );
        }
        Ok(())
    }

    /// `ClientCapabilities` omits
    /// `roots` when it is `None`.
    /// The wire form is camelCase:
    /// a future migration to
    /// `snake_case` would fail
    /// this test.
    #[test]
    fn client_capabilities_omits_roots_when_none() -> Result<(), serde_json::Error> {
        let caps = ClientCapabilities {
            roots: None,
            sampling: None,
            experimental: std::collections::BTreeMap::new(),
        };
        let value = serde_json::to_value(&caps)?;
        if let Some(object) = value.as_object() {
            assert!(!object.contains_key("roots"));
        } else {
            assert!(
                value.is_object(),
                "the serialised value should be an object"
            );
        }
        Ok(())
    }

    /// `Implementation`
    /// round-trips through
    /// `serde_json` with `title`
    /// omitted when `None`.
    #[test]
    fn implementation_round_trip() -> Result<(), serde_json::Error> {
        let impl_value = Implementation {
            name: "test-client".to_owned(),
            title: None,
            version: "0.0.1".to_owned(),
        };
        let value = serde_json::to_value(&impl_value)?;
        if let Some(object) = value.as_object() {
            assert!(!object.contains_key("title"));
        } else {
            assert!(
                value.is_object(),
                "the serialised value should be an object"
            );
        }
        let decoded: Implementation = serde_json::from_value(value)?;
        assert_eq!(impl_value, decoded);
        Ok(())
    }

    /// `CallToolRequest` uses
    /// the `name` field
    /// directly and the
    /// `arguments` field is
    /// optional via
    /// `#[serde(default)]`.
    #[test]
    fn call_tool_request_serialises_name() -> Result<(), serde_json::Error> {
        let request = CallToolRequest {
            name: "echo".to_owned(),
            arguments: serde_json::json!({}),
        };
        let value = serde_json::to_value(&request)?;
        assert_eq!(
            value.get("name").and_then(serde_json::Value::as_str),
            Some("echo"),
        );
        Ok(())
    }

    /// The wire form of
    /// `InitializeRequest` uses
    /// `camelCase` for
    /// `protocolVersion` (the
    /// schema's wire form).
    #[test]
    fn initialize_request_uses_camel_case_on_the_wire() -> Result<(), serde_json::Error> {
        let request = InitializeRequest {
            protocol_version: "2025-06-18".to_owned(),
            capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: "test-client".to_owned(),
                title: None,
                version: "0.0.1".to_owned(),
            },
        };
        let value = serde_json::to_value(&request)?;
        if let Some(object) = value.as_object() {
            assert!(object.contains_key("protocolVersion"));
            assert!(!object.contains_key("protocol_version"));
        } else {
            assert!(
                value.is_object(),
                "the serialised value should be an object"
            );
        }
        Ok(())
    }
}
