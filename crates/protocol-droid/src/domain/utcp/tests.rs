//! The domain tests, written against the specification's own examples.
//!
//! Every JSON in this file is copied from the UTCP specification's
//! documentation — the manual at `docs/for-tool-providers.md`, the call
//! templates under `docs/protocols/` — rather than written from memory. A
//! test that encodes a shape the specification does not have is a test that
//! fails for the right reason only by accident.
//!
//! The tests return `Result` rather than unwrapping: a failure here is
//! either a document the parser refused or a document it read into
//! something other than what was expected, and the two want different
//! messages.

use crate::domain::utcp::{
    ApiKeyLocation, Auth, CallTemplate, HttpMethod, Manual, ManualError, McpServer, Tool,
    Transport, UtcpVersion,
};

/// The manual the specification's provider guide shows.
const PROVIDER_MANUAL: &str = r#"{
  "manual_version": "1.0.0",
  "utcp_version": "1.0.1",
  "tools": [
    {
      "name": "get_user",
      "description": "Retrieve user information by ID",
      "inputs": {
        "type": "object",
        "properties": {
          "user_id": {"type": "string", "description": "User identifier"}
        },
        "required": ["user_id"]
      },
      "outputs": {
        "type": "object",
        "properties": {
          "id": {"type": "string"},
          "name": {"type": "string"},
          "email": {"type": "string"}
        }
      },
      "tool_call_template": {
        "name": "users",
        "call_template_type": "http",
        "url": "https://api.example.com/users/{user_id}",
        "http_method": "GET"
      }
    }
  ]
}"#;

/// Why a test failed.
#[derive(Debug)]
enum Failure {
    /// The parser refused a document.
    Manual(ManualError),
    /// The document could not be serialised.
    Json(serde_json::Error),
    /// The document parsed but did not carry what the test expected.
    Missing(&'static str),
}

impl From<ManualError> for Failure {
    fn from(error: ManualError) -> Self {
        Self::Manual(error)
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl core::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Manual(error) => write!(formatter, "the manual was refused: {error}"),
            Self::Json(error) => write!(formatter, "the manual did not serialise: {error}"),
            Self::Missing(what) => write!(formatter, "expected {what}, and it was not there"),
        }
    }
}

impl std::error::Error for Failure {}

/// The tool a test is about, or a failure saying it was not there.
fn tool_of<'a>(manual: &'a Manual, name: &'static str) -> Result<&'a Tool, Failure> {
    manual.tool(name).ok_or(Failure::Missing(name))
}

#[test]
fn a_manual_from_the_specifications_own_example_parses() -> Result<(), Failure> {
    let manual = Manual::parse(PROVIDER_MANUAL)?;
    assert_eq!(manual.manual_version, UtcpVersion::new(1, 0, 0));
    assert_eq!(manual.utcp_version, UtcpVersion::new(1, 0, 1));
    let tool = tool_of(&manual, "get_user")?;
    assert_eq!(tool.description, "Retrieve user information by ID");
    assert_eq!(tool.tags, Vec::<String>::new());
    assert!(tool.outputs.is_some());
    Ok(())
}

#[test]
fn an_http_call_template_reads_the_fields_the_specification_names() -> Result<(), Failure> {
    let manual = Manual::parse(PROVIDER_MANUAL)?;
    let tool = tool_of(&manual, "get_user")?;
    let Transport::Http(http) = &tool.tool_call_template.transport else {
        return Err(Failure::Missing("an http template"));
    };
    assert_eq!(http.url, "https://api.example.com/users/{user_id}");
    assert_eq!(http.http_method, HttpMethod::Get);
    assert!(http.body_field.is_none());
    Ok(())
}

#[test]
fn the_manual_round_trips_through_its_json_form() -> Result<(), Failure> {
    let parsed = Manual::parse(PROVIDER_MANUAL)?;
    let written = parsed.to_json()?;
    let reread = Manual::parse(&written)?;
    assert_eq!(reread, parsed);
    Ok(())
}

#[test]
fn a_tool_without_an_output_schema_keeps_it_that_way() -> Result<(), Failure> {
    let document = r#"{
      "manual_version": "1.0.0",
      "utcp_version": "1.0.1",
      "tools": [
        {
          "name": "ping",
          "description": "Check that the service is up",
          "inputs": {"type": "object", "properties": {}},
          "tool_call_template": {
            "name": "ping",
            "call_template_type": "http",
            "url": "https://api.example.com/ping",
            "http_method": "GET"
          }
        }
      ]
    }"#;
    let manual = Manual::parse(document)?;
    let tool = tool_of(&manual, "ping")?;
    assert!(tool.outputs.is_none());
    Ok(())
}

#[test]
fn a_call_template_type_this_build_does_not_know_is_kept_verbatim() -> Result<(), Failure> {
    let document = r#"{
      "manual_version": "1.0.0",
      "utcp_version": "1.0.1",
      "tools": [
        {
          "name": "stream",
          "description": "Open a websocket",
          "inputs": {"type": "object"},
          "tool_call_template": {
            "name": "stream",
            "call_template_type": "websocket",
            "url": "wss://api.example.com/stream",
            "some_future_field": {"nested": true}
          }
        }
      ]
    }"#;
    let manual = Manual::parse(document)?;
    let tool = tool_of(&manual, "stream")?;
    let Transport::Other(other) = &tool.tool_call_template.transport else {
        return Err(Failure::Missing("an unknown template type"));
    };
    assert_eq!(other.call_template_type, "websocket");
    assert!(other.raw.as_str().contains("some_future_field"));
    Ok(())
}

#[test]
fn an_authentication_type_this_build_does_not_know_is_kept_verbatim() -> Result<(), Failure> {
    let document = r#"{
      "manual_version": "1.0.0",
      "utcp_version": "1.0.1",
      "tools": [
        {
          "name": "read",
          "description": "Read a secret",
          "inputs": {"type": "object"},
          "tool_call_template": {
            "name": "read",
            "call_template_type": "http",
            "url": "https://api.example.com/secret",
            "http_method": "GET",
            "auth": {"auth_type": "mtls", "client_certificate": "${CERT}"}
          }
        }
      ]
    }"#;
    let manual = Manual::parse(document)?;
    let tool = tool_of(&manual, "read")?;
    let Some(Auth::Other { auth_type, raw }) = &tool.tool_call_template.auth else {
        return Err(Failure::Missing("an unknown auth type"));
    };
    assert_eq!(auth_type, "mtls");
    assert!(raw.as_str().contains("client_certificate"));
    Ok(())
}

#[test]
fn an_api_key_authentication_block_reads_its_three_fields() -> Result<(), Failure> {
    let document = r#"{
      "manual_version": "1.0.0",
      "utcp_version": "1.0.1",
      "tools": [
        {
          "name": "create_user",
          "description": "Create a new user account",
          "inputs": {"type": "object"},
          "tool_call_template": {
            "name": "users",
            "call_template_type": "http",
            "url": "https://api.example.com/users",
            "http_method": "POST",
            "auth": {
              "auth_type": "api_key",
              "api_key": "${API_KEY}",
              "var_name": "Authorization",
              "location": "header"
            },
            "body_field": "user_fields"
          }
        }
      ]
    }"#;
    let manual = Manual::parse(document)?;
    let tool = tool_of(&manual, "create_user")?;
    let Some(Auth::ApiKey {
        api_key,
        var_name,
        location,
    }) = &tool.tool_call_template.auth
    else {
        return Err(Failure::Missing("an api key block"));
    };
    assert_eq!(api_key, "${API_KEY}");
    assert_eq!(var_name, "Authorization");
    assert_eq!(*location, ApiKeyLocation::Header);
    let Transport::Http(http) = &tool.tool_call_template.transport else {
        return Err(Failure::Missing("an http template"));
    };
    assert_eq!(http.body_field.as_deref(), Some("user_fields"));
    Ok(())
}

#[test]
fn an_mcp_call_template_reads_the_servers_it_names() -> Result<(), Failure> {
    let document = r#"{
      "manual_version": "1.0.0",
      "utcp_version": "1.0.1",
      "tools": [
        {
          "name": "read_file",
          "description": "Read a file",
          "inputs": {"type": "object"},
          "tool_call_template": {
            "name": "mcp_tools",
            "call_template_type": "mcp",
            "config": {
              "mcpServers": {
                "tools": {
                  "command": "python",
                  "args": ["-m", "mcp_server"],
                  "cwd": "/app/mcp",
                  "env": {"LOG_LEVEL": "INFO"}
                }
              }
            }
          }
        }
      ]
    }"#;
    let manual = Manual::parse(document)?;
    let tool = tool_of(&manual, "read_file")?;
    let Transport::Mcp(mcp) = &tool.tool_call_template.transport else {
        return Err(Failure::Missing("an mcp template"));
    };
    let Some(server) = mcp.config.servers.get("tools") else {
        return Err(Failure::Missing("the named server"));
    };
    let McpServer::Stdio {
        command,
        args,
        env,
        cwd,
    } = server
    else {
        return Err(Failure::Missing("a spawned server"));
    };
    assert_eq!(command, "python");
    assert_eq!(*args, vec!["-m".to_owned(), "mcp_server".to_owned()]);
    assert_eq!(cwd.as_deref(), Some("/app/mcp"));
    assert_eq!(env.get("LOG_LEVEL").map(String::as_str), Some("INFO"));
    Ok(())
}

#[test]
fn an_mcp_call_template_reads_a_server_it_reaches_over_http() -> Result<(), Failure> {
    let document = r#"{
      "manual_version": "1.0.0",
      "utcp_version": "1.0.1",
      "tools": [
        {
          "name": "search",
          "description": "Search the corpus",
          "inputs": {"type": "object"},
          "tool_call_template": {
            "name": "secure_mcp",
            "call_template_type": "mcp",
            "config": {
              "mcpServers": {
                "secure_server": {
                  "transport": "http",
                  "url": "https://mcp.example.com"
                }
              }
            }
          }
        }
      ]
    }"#;
    let manual = Manual::parse(document)?;
    let tool = tool_of(&manual, "search")?;
    let Transport::Mcp(mcp) = &tool.tool_call_template.transport else {
        return Err(Failure::Missing("an mcp template"));
    };
    let Some(server) = mcp.config.servers.get("secure_server") else {
        return Err(Failure::Missing("the named server"));
    };
    let McpServer::Http { url, .. } = server else {
        return Err(Failure::Missing("a server reached over http"));
    };
    assert_eq!(url, "https://mcp.example.com");
    Ok(())
}

#[test]
fn a_cli_call_template_reads_its_commands() -> Result<(), Failure> {
    let document = r#"{
      "manual_version": "1.0.0",
      "utcp_version": "1.0.1",
      "tools": [
        {
          "name": "git_status",
          "description": "Show the repository status",
          "inputs": {"type": "object"},
          "tool_call_template": {
            "name": "git",
            "call_template_type": "cli",
            "commands": [
              {"command": "git status --short", "append_to_final_output": true}
            ],
            "env_vars": {"GIT_PAGER": "cat"},
            "inherit_env_vars": false,
            "working_dir": "/repo"
          }
        }
      ]
    }"#;
    let manual = Manual::parse(document)?;
    let tool = tool_of(&manual, "git_status")?;
    let Transport::Cli(cli) = &tool.tool_call_template.transport else {
        return Err(Failure::Missing("a cli template"));
    };
    assert_eq!(cli.commands.len(), 1);
    let Some(command) = cli.commands.first() else {
        return Err(Failure::Missing("the command"));
    };
    assert_eq!(command.command, "git status --short");
    assert!(command.append_to_final_output);
    assert!(!cli.inherit_env_vars);
    assert_eq!(cli.working_dir.as_deref(), Some("/repo"));
    Ok(())
}

#[test]
fn a_manual_that_is_not_a_manual_is_refused() {
    for document in [
        "not json",
        "[]",
        r#"{"tools": []}"#,
        r#"{"manual_version": "1.0.0"}"#,
        r#"{"manual_version": "one", "utcp_version": "1.0.1"}"#,
    ] {
        assert!(
            Manual::parse(document).is_err(),
            "`{document}` should not have parsed as a manual"
        );
    }
}

#[test]
fn a_tool_without_a_call_template_is_refused() {
    let document = r#"{
      "manual_version": "1.0.0",
      "utcp_version": "1.0.1",
      "tools": [{"name": "lonely", "description": "no way to call it"}]
    }"#;
    assert!(Manual::parse(document).is_err());
}

#[test]
fn the_allowed_protocols_field_is_read_when_it_is_there() -> Result<(), Failure> {
    let document = r#"{
      "manual_version": "1.1.0",
      "utcp_version": "1.1.0",
      "tools": [
        {
          "name": "read",
          "description": "Read a file",
          "inputs": {"type": "object"},
          "tool_call_template": {
            "name": "mixed",
            "call_template_type": "http",
            "url": "https://api.example.com/utcp",
            "http_method": "GET",
            "allowed_communication_protocols": ["http", "cli"]
          }
        }
      ]
    }"#;
    let manual = Manual::parse(document)?;
    let tool = tool_of(&manual, "read")?;
    assert_eq!(
        tool.tool_call_template.allowed_communication_protocols,
        Some(vec!["http".to_owned(), "cli".to_owned()])
    );
    Ok(())
}

#[test]
fn a_manual_with_no_allowed_protocols_field_keeps_it_absent() -> Result<(), Failure> {
    let manual = Manual::parse(PROVIDER_MANUAL)?;
    let tool = tool_of(&manual, "get_user")?;
    assert_eq!(tool.tool_call_template.allowed_communication_protocols, None);
    Ok(())
}

#[test]
fn a_call_template_names_its_own_type() -> Result<(), Failure> {
    let document = r#"{
      "manual_version": "1.0.0",
      "utcp_version": "1.0.1",
      "tools": [
        {
          "name": "read",
          "description": "Read a file",
          "inputs": {"type": "object"},
          "tool_call_template": {
            "name": "files",
            "call_template_type": "file",
            "file_path": "/etc/tools.json"
          }
        }
      ]
    }"#;
    let manual = Manual::parse(document)?;
    let tool = tool_of(&manual, "read")?;
    let template: &CallTemplate = &tool.tool_call_template;
    assert_eq!(template.call_template_type(), "file");
    let Transport::File(file) = &template.transport else {
        return Err(Failure::Missing("a file template"));
    };
    assert_eq!(file.file_path, "/etc/tools.json");
    Ok(())
}
