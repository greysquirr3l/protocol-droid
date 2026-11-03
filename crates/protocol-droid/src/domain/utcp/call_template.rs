//! `CallTemplate` — how a manual says to call a tool.
//!
//! # The shape of UTCP
//!
//! UTCP is not a call protocol. It is a *description* of how to call
//! something, and the client does the calling with the tool's own protocol —
//! an HTTP request, a command line, an MCP `tools/call`. The manual says
//! which, and the client obeys.
//!
//! That is why this module has no envelope, no session, and no revision
//! negotiation, and why the cross-protocol
//! [`ProtocolAdapter`](crate::application::bridge::protocol_adapter::ProtocolAdapter)
//! seam does not apply to it: that seam translates between two JSON-RPC
//! dialects, and UTCP has no dialect of its own to translate from.
//!
//! # What is modelled, and what is passed through
//!
//! The specification documents the fields of four call template types —
//! `http`, `cli`, `mcp` and `file` — and names several more without
//! spelling out their fields. Those four are modelled field by field,
//! because a client acts on every one of them. The rest are kept verbatim in
//! [`Transport::Other`]: a manual that describes a `websocket` tool is still
//! a readable manual, and guessing at fields this build has not read is how
//! a document gets mangled.
//!
//! # The `mcp` type is the interesting one
//!
//! An `mcp` call template names a set of MCP servers in the shape an MCP
//! client already uses — a command to spawn, or a URL to reach — and the
//! tool is called with `tools/call` on that server. It is the type that lets
//! a UTCP client reach an MCP server, which is how a UTCP manual describes
//! this crate's own servers.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::domain::ir::JsonObject;
use crate::domain::utcp::auth::{Auth, auth_from_map, auth_to_value};
use crate::domain::utcp::error::ManualError;
use crate::domain::utcp::parse;

/// A call template: which transport, and how to use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTemplate {
    /// The template's name. Unique within the manual it belongs to.
    pub name: String,
    /// How to authenticate, when the template says.
    pub auth: Option<Auth>,
    /// Which communication protocols a tool under this template may use.
    ///
    /// `None` means the specification's default: only the protocol the
    /// template itself names. The field arrived in v1.1, so a v1.0 manual
    /// never has it and a v1.1 manual that omits it is asking for the
    /// default.
    pub allowed_communication_protocols: Option<Vec<String>>,
    /// The transport this template describes.
    pub transport: Transport,
}

impl CallTemplate {
    /// A template that calls a tool over HTTP.
    ///
    /// The template names no credential, permits only its own protocol type,
    /// and sends no body. It is the shape a registration takes when the
    /// caller has nothing else to say.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::utcp::call_template::CallTemplate;
    /// use protocol_droid::domain::utcp::HttpMethod;
    ///
    /// let template = CallTemplate::http(
    ///     "provider",
    ///     "https://api.example.com/utcp",
    ///     HttpMethod::Get,
    /// );
    /// assert_eq!(template.call_template_type(), "http");
    /// ```
    #[must_use]
    pub fn http(name: &str, url: &str, http_method: HttpMethod) -> Self {
        Self {
            name: name.to_owned(),
            auth: None,
            allowed_communication_protocols: None,
            transport: Transport::Http(HttpTransport {
                url: url.to_owned(),
                http_method,
                content_type: None,
                headers: BTreeMap::new(),
                body_field: None,
                header_fields: Vec::new(),
            }),
        }
    }

    /// Where an HTTP call to this template goes, when it is an HTTP one.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::utcp::call_template::CallTemplate;
    /// use protocol_droid::domain::utcp::HttpMethod;
    ///
    /// let template = CallTemplate::http("t", "https://api.example.com/x", HttpMethod::Get);
    /// assert_eq!(template.http_url().as_deref(), Some("https://api.example.com/x"));
    /// ```
    #[must_use]
    pub fn http_url(&self) -> Option<String> {
        match &self.transport {
            Transport::Http(http) => Some(http.url.clone()),
            _ => None,
        }
    }

    /// The MCP servers this template names, when it is an MCP one.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::utcp::call_template::CallTemplate;
    /// use protocol_droid::domain::utcp::HttpMethod;
    ///
    /// let template = CallTemplate::http("t", "https://api.example.com/x", HttpMethod::Get);
    /// assert!(template.mcp_servers().is_empty());
    /// ```
    #[must_use]
    pub fn mcp_servers(&self) -> Vec<String> {
        match &self.transport {
            Transport::Mcp(mcp) => mcp.config.servers.keys().cloned().collect(),
            _ => Vec::new(),
        }
    }

    /// Reads a call template out of a parsed document.
    ///
    /// # Errors
    ///
    /// [`ManualError::MissingField`] when the discriminator or a required
    /// field is absent, and [`ManualError::InvalidField`] when one is
    /// present but is not the right shape.
    pub fn from_value(value: &Value) -> Result<Self, ManualError> {
        let object = parse::object(value, "call template")?;
        let call_template_type = parse::required_str(object, "call template", "call_template_type")?;
        let transport = match call_template_type.as_str() {
            "http" => Transport::Http(HttpTransport::from_object(object)?),
            "cli" => Transport::Cli(CliTransport::from_object(object)?),
            "mcp" => Transport::Mcp(McpTransport::from_object(object)?),
            "file" => Transport::File(FileTransport::from_object(object)?),
            _ => Transport::Other(OtherTransport {
                call_template_type: call_template_type.clone(),
                raw: JsonObject::from_value(value.clone()).map_err(|error| {
                    ManualError::InvalidField {
                        what: "call template",
                        field: "call_template_type",
                        detail: error.to_string(),
                    }
                })?,
            }),
        };
        Ok(Self {
            name: parse::required_str(object, "call template", "name")?,
            auth: auth_from_map(object)?,
            allowed_communication_protocols: match object.get("allowed_communication_protocols") {
                None | Some(Value::Null) => None,
                Some(_) => Some(parse::string_array(
                    object,
                    "call template",
                    "allowed_communication_protocols",
                )?),
            },
            transport,
        })
    }

    /// The `call_template_type` this template carries.
    #[must_use]
    pub fn call_template_type(&self) -> &str {
        self.transport.type_name()
    }

    /// The transport this template describes.
    #[must_use]
    pub const fn transport(&self) -> &Transport {
        &self.transport
    }
}

/// The transport a call template describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// An HTTP request.
    Http(HttpTransport),
    /// A command line.
    Cli(CliTransport),
    /// An MCP server reached with `tools/call`.
    Mcp(McpTransport),
    /// A file on disk.
    File(FileTransport),
    /// A transport this build does not model, kept as it arrived.
    Other(OtherTransport),
}

impl Transport {
    /// The `call_template_type` spelling of this transport.
    #[must_use]
    pub fn type_name(&self) -> &str {
        match self {
            Self::Http(_) => "http",
            Self::Cli(_) => "cli",
            Self::Mcp(_) => "mcp",
            Self::File(_) => "file",
            Self::Other(other) => &other.call_template_type,
        }
    }
}

/// An HTTP call template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpTransport {
    /// The URL, with `{parameter}` placeholders for path parameters.
    pub url: String,
    /// The method to use.
    pub http_method: HttpMethod,
    /// The `Content-Type` to send, when the template names one.
    pub content_type: Option<String>,
    /// Headers sent on every request.
    pub headers: BTreeMap<String, String>,
    /// The argument whose value is the request body.
    pub body_field: Option<String>,
    /// The arguments whose values are request headers.
    pub header_fields: Vec<String>,
}

impl HttpTransport {
    /// Reads an HTTP transport out of a parsed call template.
    ///
    /// # Errors
    ///
    /// [`ManualError::MissingField`] when the URL or the method is absent,
    /// and [`ManualError::InvalidField`] when a field is the wrong shape.
    fn from_object(object: &Map<String, Value>) -> Result<Self, ManualError> {
        let method = parse::required_str(object, "call template", "http_method")?;
        Ok(Self {
            url: parse::required_str(object, "call template", "url")?,
            http_method: HttpMethod::from_str(&method),
            content_type: parse::optional_str(object, "content_type"),
            headers: parse::string_map(object, "call template", "headers")?,
            body_field: parse::optional_str(object, "body_field"),
            header_fields: parse::string_array(object, "call template", "header_fields")?,
        })
    }
}

/// The HTTP method a call template names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpMethod {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `PUT`.
    Put,
    /// `PATCH`.
    Patch,
    /// `DELETE`.
    Delete,
    /// A method this build does not name, kept as it arrived.
    Other(String),
}

impl HttpMethod {
    /// The spelling the specification uses.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Other(name) => name,
        }
    }

    /// Reads a method out of the document.
    ///
    /// A method this build does not name is kept rather than refused,
    /// because a method is a token and the peer may have a reason for it.
    fn from_str(raw: &str) -> Self {
        match raw {
            "GET" => Self::Get,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "PATCH" => Self::Patch,
            "DELETE" => Self::Delete,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// A CLI call template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliTransport {
    /// The commands to run, in order.
    pub commands: Vec<CliCommand>,
    /// Environment variables to set for the commands.
    pub env_vars: BTreeMap<String, String>,
    /// Whether the commands inherit the host's environment.
    pub inherit_env_vars: bool,
    /// The directory to run the commands in.
    pub working_dir: Option<String>,
}

impl CliTransport {
    /// Reads a CLI transport out of a parsed call template.
    ///
    /// # Errors
    ///
    /// [`ManualError::InvalidField`] when the commands or the environment
    /// block is not the right shape.
    fn from_object(object: &Map<String, Value>) -> Result<Self, ManualError> {
        let commands = match object.get("commands") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(CliCommand::from_value)
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => {
                return Err(ManualError::InvalidField {
                    what: "call template",
                    field: "commands",
                    detail: "it is not a list".to_owned(),
                });
            }
        };
        Ok(Self {
            commands,
            env_vars: parse::string_map(object, "call template", "env_vars")?,
            inherit_env_vars: object
                .get("inherit_env_vars")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            working_dir: parse::optional_str(object, "working_dir"),
        })
    }
}

/// One step of a CLI call template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliCommand {
    /// The command line to run.
    pub command: String,
    /// Whether this step's output joins the final output.
    pub append_to_final_output: bool,
}

impl CliCommand {
    /// Reads a command step out of a parsed document.
    ///
    /// # Errors
    ///
    /// [`ManualError::InvalidField`] when the step is not an object or has no
    /// command.
    fn from_value(value: &Value) -> Result<Self, ManualError> {
        let object = parse::object(value, "command step")?;
        Ok(Self {
            command: parse::required_str(object, "command step", "command")?,
            append_to_final_output: object
                .get("append_to_final_output")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// An MCP call template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTransport {
    /// The MCP servers the template names.
    pub config: McpConfig,
}

impl McpTransport {
    /// Reads an MCP transport out of a parsed call template.
    ///
    /// # Errors
    ///
    /// [`ManualError::MissingField`] when the `config` block is absent, and
    /// [`ManualError::InvalidField`] when it is not the right shape.
    fn from_object(object: &Map<String, Value>) -> Result<Self, ManualError> {
        let config = object
            .get("config")
            .ok_or(ManualError::MissingField {
                what: "call template",
                field: "config",
            })?;
        Ok(Self {
            config: McpConfig::from_value(config)?,
        })
    }
}

/// The MCP servers a call template names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpConfig {
    /// The servers, by name.
    pub servers: BTreeMap<String, McpServer>,
}

impl McpConfig {
    /// Reads the server block out of a parsed document.
    ///
    /// # Errors
    ///
    /// [`ManualError::InvalidField`] when the block is not an object or one
    /// of the servers is malformed.
    fn from_value(value: &Value) -> Result<Self, ManualError> {
        let object = parse::object(value, "MCP configuration")?;
        let servers = match object.get("mcpServers") {
            None | Some(Value::Null) => BTreeMap::new(),
            Some(Value::Object(entries)) => entries
                .iter()
                .map(|(name, server)| Ok((name.clone(), McpServer::from_value(server)?)))
                .collect::<Result<BTreeMap<_, _>, ManualError>>()?,
            Some(_) => {
                return Err(ManualError::InvalidField {
                    what: "MCP configuration",
                    field: "mcpServers",
                    detail: "it is not an object".to_owned(),
                });
            }
        };
        Ok(Self { servers })
    }
}

/// One MCP server a call template names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServer {
    /// A server this process spawns.
    Stdio {
        /// The command to run.
        command: String,
        /// The arguments to run it with.
        args: Vec<String>,
        /// Environment variables to set for it.
        env: BTreeMap<String, String>,
        /// The directory to run it in.
        cwd: Option<String>,
    },
    /// A server this process reaches over HTTP.
    Http {
        /// Where the server is.
        url: String,
        /// Headers to send to it.
        headers: BTreeMap<String, String>,
        /// The bearer token this server is reached with.
        ///
        /// The specification: a server entry's own credential takes
        /// precedence over the manual-level one. `None` means the entry
        /// names none, and the manual-level credential applies.
        auth_token: Option<String>,
    },
    /// A server configuration this build does not model, kept as it arrived.
    Other(JsonObject),
}

impl McpServer {
    /// Reads a server out of a parsed document.
    ///
    /// # Errors
    ///
    /// [`ManualError::InvalidField`] when the server is not an object or is
    /// missing the field that says how to reach it.
    fn from_value(value: &Value) -> Result<Self, ManualError> {
        let object = parse::object(value, "MCP server")?;
        let has_command = object.contains_key("command");
        let has_url = object.contains_key("url");
        match (has_command, has_url) {
            (true, _) => Ok(Self::Stdio {
                command: parse::required_str(object, "MCP server", "command")?,
                args: parse::string_array(object, "MCP server", "args")?,
                env: parse::string_map(object, "MCP server", "env")?,
                cwd: parse::optional_str(object, "cwd"),
            }),
            (false, true) => Ok(Self::Http {
                url: parse::required_str(object, "MCP server", "url")?,
                headers: parse::string_map(object, "MCP server", "headers")?,
                auth_token: parse::optional_str(object, "auth_token"),
            }),
            (false, false) => Err(ManualError::InvalidField {
                what: "MCP server",
                field: "command",
                detail: "it names neither a command to spawn nor a url to reach".to_owned(),
            }),
        }
    }
}

/// A file call template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransport {
    /// The path of the file that holds the tool's definition.
    pub file_path: String,
}

impl FileTransport {
    /// Reads a file transport out of a parsed call template.
    ///
    /// # Errors
    ///
    /// [`ManualError::MissingField`] when the path is absent.
    fn from_object(object: &Map<String, Value>) -> Result<Self, ManualError> {
        Ok(Self {
            file_path: parse::required_str(object, "call template", "file_path")?,
        })
    }
}

/// A transport this build does not model, kept as it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherTransport {
    /// The `call_template_type` discriminator.
    pub call_template_type: String,
    /// The whole template, verbatim.
    pub raw: JsonObject,
}

/// Writes a call template back out in the shape the document carried it.
pub fn template_to_value(template: &CallTemplate) -> Value {
    let mut object = Map::new();
    object.insert(
        "name".to_owned(),
        Value::String(template.name.clone()),
    );
    object.insert(
        "call_template_type".to_owned(),
        Value::String(template.call_template_type().to_owned()),
    );
    if let Some(auth) = auth_to_value(&template.auth) {
        object.insert("auth".to_owned(), auth);
    }
    if let Some(allowed) = &template.allowed_communication_protocols {
        object.insert(
            "allowed_communication_protocols".to_owned(),
            Value::Array(
                allowed
                    .iter()
                    .map(|name| Value::String(name.clone()))
                    .collect(),
            ),
        );
    }
    write_transport(&mut object, &template.transport);
    Value::Object(object)
}

/// Writes a transport's own fields into the object.
///
/// A transport this build does not model carries its whole body verbatim,
/// discriminator included, so it replaces what has been built rather than
/// merging with it.
fn write_transport(object: &mut Map<String, Value>, transport: &Transport) {
    match transport {
        Transport::Http(http) => write_http(object, http),
        Transport::Cli(cli) => write_cli(object, cli),
        Transport::Mcp(mcp) => write_mcp(object, mcp),
        Transport::File(file) => {
            object.insert(
                "file_path".to_owned(),
                Value::String(file.file_path.clone()),
            );
        }
        Transport::Other(other) => {
            // The raw body is the whole template, discriminator included, so
            // it replaces what this function has built rather than merging
            // with it.
            *object = serde_json::from_str(other.raw.as_str())
                .ok()
                .and_then(|value| match value {
                    Value::Object(map) => Some(map),
                    _ => None,
                })
                .unwrap_or_default();
        }
    }
}

/// Writes an HTTP transport's fields.
fn write_http(object: &mut Map<String, Value>, http: &HttpTransport) {
    object.insert("url".to_owned(), Value::String(http.url.clone()));
    object.insert(
        "http_method".to_owned(),
        Value::String(http.http_method.as_str().to_owned()),
    );
    if let Some(content_type) = &http.content_type {
        object.insert(
            "content_type".to_owned(),
            Value::String(content_type.clone()),
        );
    }
    if !http.headers.is_empty() {
        object.insert(
            "headers".to_owned(),
            Value::Object(
                http.headers
                    .iter()
                    .map(|(name, value)| (name.clone(), Value::String(value.clone())))
                    .collect(),
            ),
        );
    }
    if let Some(body_field) = &http.body_field {
        object.insert(
            "body_field".to_owned(),
            Value::String(body_field.clone()),
        );
    }
    if !http.header_fields.is_empty() {
        object.insert(
            "header_fields".to_owned(),
            Value::Array(
                http.header_fields
                    .iter()
                    .map(|name| Value::String(name.clone()))
                    .collect(),
            ),
        );
    }
}

/// Writes a CLI transport's fields.
fn write_cli(object: &mut Map<String, Value>, cli: &CliTransport) {
    object.insert(
        "commands".to_owned(),
        Value::Array(
            cli.commands
                .iter()
                .map(|command| {
                    let mut step = Map::new();
                    step.insert(
                        "command".to_owned(),
                        Value::String(command.command.clone()),
                    );
                    step.insert(
                        "append_to_final_output".to_owned(),
                        Value::Bool(command.append_to_final_output),
                    );
                    Value::Object(step)
                })
                .collect(),
        ),
    );
    if !cli.env_vars.is_empty() {
        object.insert(
            "env_vars".to_owned(),
            Value::Object(
                cli.env_vars
                    .iter()
                    .map(|(name, value)| (name.clone(), Value::String(value.clone())))
                    .collect(),
            ),
        );
    }
    object.insert(
        "inherit_env_vars".to_owned(),
        Value::Bool(cli.inherit_env_vars),
    );
    if let Some(working_dir) = &cli.working_dir {
        object.insert(
            "working_dir".to_owned(),
            Value::String(working_dir.clone()),
        );
    }
}

/// Writes an MCP transport's fields.
fn write_mcp(object: &mut Map<String, Value>, mcp: &McpTransport) {
    let mut servers = Map::new();
    for (name, server) in &mcp.config.servers {
        let entry = match server {
            McpServer::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                let mut entry = Map::new();
                entry.insert("command".to_owned(), Value::String(command.clone()));
                if !args.is_empty() {
                    entry.insert(
                        "args".to_owned(),
                        Value::Array(
                            args.iter()
                                .map(|arg| Value::String(arg.clone()))
                                .collect(),
                        ),
                    );
                }
                if !env.is_empty() {
                    entry.insert(
                        "env".to_owned(),
                        Value::Object(
                            env.iter()
                                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                                .collect(),
                        ),
                    );
                }
                if let Some(cwd) = cwd {
                    entry.insert("cwd".to_owned(), Value::String(cwd.clone()));
                }
                Value::Object(entry)
            }
            McpServer::Http {
                url,
                headers,
                auth_token,
            } => {
                let mut entry = Map::new();
                entry.insert("url".to_owned(), Value::String(url.clone()));
                if let Some(token) = auth_token {
                    entry.insert("auth_token".to_owned(), Value::String(token.clone()));
                }
                if !headers.is_empty() {
                    entry.insert(
                        "headers".to_owned(),
                        Value::Object(
                            headers
                                .iter()
                                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                                .collect(),
                        ),
                    );
                }
                Value::Object(entry)
            }
            McpServer::Other(raw) => serde_json::from_str(raw.as_str()).unwrap_or(Value::Null),
        };
        servers.insert(name.clone(), entry);
    }
    let mut config = Map::new();
    config.insert("mcpServers".to_owned(), Value::Object(servers));
    object.insert("config".to_owned(), Value::Object(config));
}
