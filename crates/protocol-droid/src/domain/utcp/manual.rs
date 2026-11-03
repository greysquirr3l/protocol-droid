//! `Manual` and `Tool` — the document a UTCP provider publishes.
//!
//! # What a manual is
//!
//! A UTCP provider publishes one JSON document at `GET /utcp` that lists
//! every tool it offers and says how to call each one. The document is the
//! whole protocol on the provider's side: there is no session, no
//! negotiation, and no request the provider has to answer.
//!
//! Two versions travel in it. `utcp_version` is the protocol revision the
//! provider speaks, and `manual_version` is the revision of the document
//! format itself. They are separate because the document format can move
//! without the protocol moving — v1.1 of the protocol added a field to every
//! call template and nothing else.
//!
//! # Why the schemas are kept as text
//!
//! A tool's `inputs` and `outputs` are JSON Schema. This crate does not
//! model JSON Schema, and modelling it would be wrong twice over: the
//! specification's schema vocabulary is large and still growing, and a tool
//! that needs a keyword this build has never heard of should still work. So
//! the schemas are held as [`JsonObject`] — validated once, at the boundary,
//! and stored as the text they arrived as.
//!
//! # Why a description is a `String` and not an `Option`
//!
//! The specification types `description` as a plain string, and a manual
//! without one is malformed rather than sparse. A missing description is
//! read as the empty string, which is what the document's own default is,
//! and which is what a reader would have written in its place.

use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

use crate::domain::ir::JsonObject;
use crate::domain::utcp::call_template::{CallTemplate, template_to_value};
use crate::domain::utcp::error::ManualError;
use crate::domain::utcp::parse;
use crate::domain::utcp::version::UtcpVersion;

/// A UTCP manual.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manual {
    /// The revision of the document format.
    pub manual_version: UtcpVersion,
    /// The revision of the protocol.
    pub utcp_version: UtcpVersion,
    /// The tools the provider offers.
    pub tools: Vec<Tool>,
}

impl Manual {
    /// A manual with the two versions and the tools given.
    #[must_use]
    pub const fn new(
        manual_version: UtcpVersion,
        utcp_version: UtcpVersion,
        tools: Vec<Tool>,
    ) -> Self {
        Self {
            manual_version,
            utcp_version,
            tools,
        }
    }

    /// Reads a manual out of a JSON document.
    ///
    /// # Errors
    ///
    /// [`ManualError::InvalidJson`] when the text is not JSON,
    /// [`ManualError::NotAnObject`] when it is not an object, and
    /// [`ManualError::MissingField`] or [`ManualError::InvalidField`] when a
    /// part of it is not what the specification says.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::utcp::Manual;
    ///
    /// let manual = Manual::parse(r#"{
    ///   "manual_version": "1.0.0",
    ///   "utcp_version": "1.0.1",
    ///   "tools": []
    /// }"#)?;
    /// assert_eq!(manual.tools.len(), 0);
    /// # Ok::<(), protocol_droid::domain::utcp::ManualError>(())
    /// ```
    pub fn parse(text: &str) -> Result<Self, ManualError> {
        let value =
            serde_json::from_str(text).map_err(|error| ManualError::InvalidJson {
                detail: error.to_string(),
            })?;
        Self::from_value(&value)
    }

    /// Reads a manual out of an already-parsed document.
    ///
    /// # Errors
    ///
    /// As [`Manual::parse`].
    pub fn from_value(value: &Value) -> Result<Self, ManualError> {
        let object = parse::object(value, "manual")?;
        let tools = match object.get("tools") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(Tool::from_value)
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => {
                return Err(ManualError::InvalidField {
                    what: "manual",
                    field: "tools",
                    detail: "it is not a list".to_owned(),
                });
            }
        };
        Ok(Self {
            manual_version: parse::required_version(object, "manual", "manual_version")?,
            utcp_version: parse::required_version(object, "manual", "utcp_version")?,
            tools,
        })
    }

    /// The manual as a JSON document.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        object.insert(
            "manual_version".to_owned(),
            Value::String(self.manual_version.to_string()),
        );
        object.insert(
            "utcp_version".to_owned(),
            Value::String(self.utcp_version.to_string()),
        );
        object.insert(
            "tools".to_owned(),
            Value::Array(self.tools.iter().map(Tool::to_value).collect()),
        );
        Value::Object(object)
    }

    /// The manual as JSON text.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] when the document cannot be serialised, which
    /// for a manual built from these types cannot happen.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// The tool with the given name.
    #[must_use]
    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|tool| tool.name == name)
    }
}

impl Serialize for Manual {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

/// One tool a manual describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// The tool's name. Unique within the manual.
    pub name: String,
    /// What the tool does.
    pub description: String,
    /// The tool's input schema, as a JSON Schema.
    pub inputs: JsonObject,
    /// The tool's output schema, when it has one.
    pub outputs: Option<JsonObject>,
    /// The tags the tool is filed under.
    pub tags: Vec<String>,
    /// A hint at how big a response is, in bytes.
    pub average_response_size: Option<u64>,
    /// How to call the tool.
    pub tool_call_template: CallTemplate,
}

impl Tool {
    /// A tool with a name and a call template, and nothing else.
    ///
    /// The input schema is the empty object schema, which accepts anything —
    /// the same thing MCP's `inputSchema` says when a tool takes no
    /// arguments.
    #[must_use]
    pub fn new(name: impl Into<String>, tool_call_template: CallTemplate) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            inputs: empty_schema(),
            outputs: None,
            tags: Vec::new(),
            average_response_size: None,
            tool_call_template,
        }
    }

    /// A tool with a description.
    #[must_use]
    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// A tool with tags.
    #[must_use]
    pub fn tagged(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.tags = tags.into_iter().collect();
        self
    }

    /// Reads a tool out of a parsed document.
    ///
    /// # Errors
    ///
    /// [`ManualError::MissingField`] when the name or the call template is
    /// absent, and [`ManualError::InvalidField`] when a field is the wrong
    /// shape.
    pub fn from_value(value: &Value) -> Result<Self, ManualError> {
        let object = parse::object(value, "tool")?;
        let template = object
            .get("tool_call_template")
            .ok_or(ManualError::MissingField {
                what: "tool",
                field: "tool_call_template",
            })?;
        Ok(Self {
            name: parse::required_str(object, "tool", "name")?,
            description: parse::optional_str(object, "description").unwrap_or_default(),
            inputs: parse::optional_json_object(object, "tool", "inputs")?
                .unwrap_or_else(empty_schema),
            outputs: parse::optional_json_object(object, "tool", "outputs")?,
            tags: parse::string_array(object, "tool", "tags")?,
            average_response_size: object.get("average_response_size").and_then(Value::as_u64),
            tool_call_template: CallTemplate::from_value(template)?,
        })
    }

    /// The tool as a JSON document.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("name".to_owned(), Value::String(self.name.clone()));
        object.insert(
            "description".to_owned(),
            Value::String(self.description.clone()),
        );
        object.insert("inputs".to_owned(), schema_to_value(&self.inputs));
        if let Some(outputs) = &self.outputs {
            object.insert("outputs".to_owned(), schema_to_value(outputs));
        }
        object.insert(
            "tags".to_owned(),
            Value::Array(
                self.tags
                    .iter()
                    .map(|tag| Value::String(tag.clone()))
                    .collect(),
            ),
        );
        if let Some(size) = self.average_response_size {
            object.insert("average_response_size".to_owned(), Value::from(size));
        }
        object.insert(
            "tool_call_template".to_owned(),
            template_to_value(&self.tool_call_template),
        );
        Value::Object(object)
    }
}

impl Serialize for Tool {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

/// The schema a tool with no input schema is given.
fn empty_schema() -> JsonObject {
    // The literal is a JSON object by construction, so the parse cannot
    // fail; the fallback keeps the constructor infallible rather than
    // wrapping a constant in a Result the caller would have to unwrap.
    JsonObject::new(r#"{"type":"object"}"#).unwrap_or_default()
}

/// A schema as a JSON document.
fn schema_to_value(schema: &JsonObject) -> Value {
    serde_json::from_str(schema.as_str()).unwrap_or_else(|_| Value::Object(Map::new()))
}
