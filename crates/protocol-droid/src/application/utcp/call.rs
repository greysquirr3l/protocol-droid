//! Planning a call from a manual's tool.
//!
//! # What this module decides
//!
//! A manual says *what* a tool is and *how* it is reached; this module turns
//! that into a concrete call: the URL, the headers, the body, and the
//! credential to present. It is a pure function of the tool, the arguments,
//! and the variables — no socket, no clock, no environment — so every rule
//! below is testable without one.
//!
//! # The rules, in the specification's order
//!
//! The UTCP HTTP protocol handles a tool's arguments in four steps, and the
//! order matters because an argument can be claimed by more than one of
//! them:
//!
//! 1. **Path parameters.** An argument named `{param}` in the URL is
//!    substituted into it.
//! 2. **Body.** The argument named by `body_field` is the request body.
//! 3. **Headers.** The arguments named by `header_fields` are request
//!    headers.
//! 4. **Query.** Everything left over is a query parameter.
//!
//! An argument claimed by an earlier step is not sent again by a later one.
//! A URL that already carries a query string keeps it.
//!
//! # Why the encoding is RFC 3986's unreserved set
//!
//! A substituted value is caller-influenced in the general case, so it is
//! percent-encoded down to the unreserved characters (`A-Z`, `a-z`, `0-9`,
//! `-`, `.`, `_`, `~`). Anything looser would let a value containing `&` or
//! `/` change the shape of the request rather than be part of one.
//!
//! # Why an MCP tool name is split on its first dot
//!
//! The specification's MCP plugin registers a tool as `<server>.<tool>` and
//! splits it back that way to find the server the call goes to. A name
//! whose prefix does not name a server in the template is not split at all:
//! a tool called `report.builder` on a template with one server is that
//! server's tool, not a server called `report`.

use std::collections::BTreeMap;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Map, Value};

use crate::domain::ir::JsonObject;
use crate::domain::utcp::call_template::{HttpMethod, HttpTransport, McpServer, McpTransport, Transport};
use crate::domain::utcp::policy::ProtocolPolicy;
use crate::domain::utcp::variables::{VariableError, substitute};
use crate::domain::utcp::{ApiKeyLocation, Auth, Tool};

/// Everything outside RFC 3986's unreserved set is encoded.
///
/// `NON_ALPHANUMERIC` encodes `-`, `.`, `_` and `~` as well, which are legal
/// in a path segment and in a query value; removing them leaves exactly the
/// unreserved set.
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// A call a client is about to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    /// An HTTP request.
    Http(HttpCall),
    /// An MCP `tools/call`.
    Mcp(McpCall),
}

/// An HTTP request a tool's call template describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpCall {
    /// The method to use.
    pub method: HttpMethod,
    /// The URL, with the path parameters substituted and the leftover
    /// arguments already on the query.
    pub url: String,
    /// The headers to send.
    pub headers: BTreeMap<String, String>,
    /// The body, as JSON text, when the template names a body field.
    pub body: Option<String>,
    /// The credential to present.
    pub auth: HttpAuth,
}

/// The credential an HTTP call presents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpAuth {
    /// No credential.
    None,
    /// A key, sent in a header, a query parameter, or a cookie.
    ApiKey {
        /// The name of the header, parameter, or cookie.
        var_name: String,
        /// Where it travels.
        location: ApiKeyLocation,
        /// The key itself, with its variables already resolved.
        value: String,
    },
    /// A username and a password.
    Basic {
        /// The username, with its variables already resolved.
        username: String,
        /// The password, with its variables already resolved.
        password: String,
    },
    /// An `OAuth2` client-credentials exchange, which the adapter performs
    /// before the request goes out.
    OAuth2 {
        /// Where the token is fetched from.
        token_url: String,
        /// The client identifier.
        client_id: String,
        /// The client secret.
        client_secret: String,
        /// The scope to ask for.
        scope: Option<String>,
    },
}

/// An MCP `tools/call` a tool's call template describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCall {
    /// The name of the server the call goes to.
    pub server_name: String,
    /// How to reach that server.
    pub server: McpServer,
    /// The tool's name on that server.
    pub tool: String,
    /// The arguments, unchanged.
    pub arguments: JsonObject,
    /// The credential to present, when the template names one.
    pub auth: Option<Auth>,
}

/// Why a call could not be planned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// The call template names a transport this build cannot call.
    #[error("a `{call_template_type}` call template cannot be called by this build")]
    UnsupportedTransport {
        /// The transport the template names.
        call_template_type: String,
    },
    /// The URL has a path parameter and no argument fills it.
    #[error("the URL needs a `{name}` argument and none was given")]
    MissingArgument {
        /// The name of the missing argument.
        name: String,
    },
    /// An argument that has to be a string is not one.
    #[error("the `{name}` argument has to be a string")]
    NotAString {
        /// The name of the argument.
        name: String,
    },
    /// An argument that has to be a single value is a structure.
    #[error("the `{name}` argument has to be a single value")]
    NotASingleValue {
        /// The name of the argument.
        name: String,
    },
    /// The arguments are not a JSON object.
    #[error("the arguments are not a JSON object")]
    NotAnObject,
    /// The template names a server that is not in its configuration.
    #[error("the template has no server called `{name}`")]
    UnknownServer {
        /// The name of the server.
        name: String,
    },
    /// The registration does not permit the tool's protocol.
    ///
    /// The specification's `allowed_communication_protocols` filter, applied
    /// at call time. The message names what was permitted so the reader can
    /// see the gap rather than guess at it.
    #[error(
        "the tool uses the `{protocol}` protocol, which this registration does not permit; it permits {allowed:?}"
    )]
    ProtocolNotPermitted {
        /// The protocol the tool wanted to be called over.
        protocol: String,
        /// The protocol types the registration permits.
        allowed: Vec<String>,
    },
    /// The template names several servers and the tool does not say which.
    #[error("the template names several servers and `{tool}` does not say which")]
    AmbiguousServer {
        /// The tool's name.
        tool: String,
    },
    /// A variable in the template is not set.
    #[error(transparent)]
    Variable(#[from] VariableError),
}

/// Plans the call for a tool.
///
/// The policy is checked before the plan is made. A tool whose protocol the
/// registration does not permit is refused here, even if the caller reached
/// this function with the tool in hand: registration filters what is
/// *offered*, and this filters what is *done*.
///
/// # Errors
///
/// [`CallError`] when the policy does not permit the tool's protocol, when
/// the template names a transport this build cannot call, when an argument
/// the URL needs is missing, when an argument that has to be a string is not
/// one, when the template names several MCP servers and the tool does not
/// say which, or when a variable the template refers to is not set.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
///
/// use protocol_droid::application::utcp::call::{HttpAuth, HttpCall, ToolCall, plan_call};
/// use protocol_droid::domain::ir::JsonObject;
/// use protocol_droid::domain::utcp::policy::ProtocolPolicy;
/// use protocol_droid::domain::utcp::{HttpMethod, Manual};
///
/// let manual = Manual::parse(r#"{
///   "manual_version": "1.0.0",
///   "utcp_version": "1.0.1",
///   "tools": [{
///     "name": "get_user",
///     "description": "Fetch a user",
///     "inputs": {"type": "object"},
///     "tool_call_template": {
///       "name": "users",
///       "call_template_type": "http",
///       "url": "https://api.example.com/users/{user_id}",
///       "http_method": "GET"
///     }
///   }]
/// }"#)?;
/// let Some(tool) = manual.tool("get_user") else {
///     return Err("the manual has no `get_user` tool".into());
/// };
/// let arguments = JsonObject::new(r#"{"user_id": "42"}"#)?;
/// let policy = ProtocolPolicy::allowing(["http".to_owned()]);
/// let call = plan_call(tool, &arguments, &BTreeMap::new(), &policy)?;
/// assert_eq!(
///     call,
///     ToolCall::Http(HttpCall {
///         method: HttpMethod::Get,
///         url: "https://api.example.com/users/42".to_owned(),
///         headers: BTreeMap::new(),
///         body: None,
///         auth: HttpAuth::None,
///     })
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn plan_call(
    tool: &Tool,
    arguments: &JsonObject,
    variables: &BTreeMap<String, String>,
    policy: &ProtocolPolicy,
) -> Result<ToolCall, CallError> {
    let protocol = tool.tool_call_template.call_template_type();
    if !policy.permits(protocol) {
        return Err(CallError::ProtocolNotPermitted {
            protocol: protocol.to_owned(),
            allowed: policy.allowed(),
        });
    }
    match &tool.tool_call_template.transport {
        Transport::Http(http) => plan_http(tool, http, arguments, variables).map(ToolCall::Http),
        Transport::Mcp(mcp) => plan_mcp(tool, mcp, arguments, variables).map(ToolCall::Mcp),
        other => Err(CallError::UnsupportedTransport {
            call_template_type: other.type_name().to_owned(),
        }),
    }
}

/// Plans an HTTP call.
fn plan_http(
    tool: &Tool,
    template: &HttpTransport,
    arguments: &JsonObject,
    variables: &BTreeMap<String, String>,
) -> Result<HttpCall, CallError> {
    let mut supplied = arguments_of(arguments)?;
    // The specification's order, and it matters: an argument is claimed by
    // the first step that claims it. The URL's placeholders come first, then
    // the body, then the headers, and whatever is left over is the query.
    let (path, query) = split_url(&template.url);
    let path = substitute_placeholders(path, &mut supplied)?;
    let query = match query {
        Some(query) => Some(substitute_placeholders(query, &mut supplied)?),
        None => None,
    };
    let body = take_body(template, &mut supplied);
    let mut headers = take_headers(template, &mut supplied)?;
    let url = build_url(path, query, &supplied);

    // The template's own headers are the provider's declaration, so they go
    // in first and the caller's own header arguments overwrite them: the
    // more specific value wins.
    for (name, value) in &template.headers {
        headers.insert(name.clone(), substitute(value, variables)?);
    }

    Ok(HttpCall {
        method: template.http_method.clone(),
        url,
        headers,
        body,
        auth: http_auth(tool.tool_call_template.auth.as_ref(), variables)?,
    })
}

/// The arguments, as a map.
fn arguments_of(arguments: &JsonObject) -> Result<Map<String, Value>, CallError> {
    let value =
        serde_json::from_str::<Value>(arguments.as_str()).map_err(|_| CallError::NotAnObject)?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(CallError::NotAnObject),
    }
}

/// Splits a URL into its path and its query.
fn split_url(template: &str) -> (&str, Option<&str>) {
    match template.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (template, None),
    }
}

/// Builds the final URL from the substituted path, the query the template
/// carried, and the arguments that are left over.
fn build_url(path: String, query: Option<String>, supplied: &Map<String, Value>) -> String {
    let mut query_string = query.unwrap_or_default();
    for (name, value) in remaining_query(supplied) {
        if !query_string.is_empty() {
            query_string.push('&');
        }
        query_string.push_str(&encode(&name));
        query_string.push('=');
        query_string.push_str(&encode(&value));
    }
    if query_string.is_empty() {
        path
    } else {
        format!("{path}?{query_string}")
    }
}

/// Substitutes `{name}` placeholders, taking the arguments it uses out of
/// `supplied` so that they are not sent a second time as query parameters.
fn substitute_placeholders(
    text: &str,
    supplied: &mut Map<String, Value>,
) -> Result<String, CallError> {
    let mut resolved = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start + 1..].find('}') else {
            break;
        };
        let name = &rest[start + 1..start + 1 + end];
        if name.is_empty() {
            break;
        }
        let Some(value) = supplied.remove(name) else {
            return Err(CallError::MissingArgument {
                name: name.to_owned(),
            });
        };
        let encoded = match value {
            Value::String(text) => encode(&text),
            Value::Number(number) => number.to_string(),
            Value::Bool(flag) => flag.to_string(),
            _ => {
                return Err(CallError::NotASingleValue {
                    name: name.to_owned(),
                });
            }
        };
        resolved.push_str(&rest[..start]);
        resolved.push_str(&encoded);
        rest = &rest[start + 1 + end + 1..];
    }
    resolved.push_str(rest);
    Ok(resolved)
}

/// Percent-encodes a value down to the unreserved set.
fn encode(text: &str) -> String {
    utf8_percent_encode(text, UNRESERVED).to_string()
}

/// Takes the body argument out of the arguments.
fn take_body(
    template: &HttpTransport,
    supplied: &mut Map<String, Value>,
) -> Option<String> {
    let Some(field) = &template.body_field else {
        return None;
    };
    let value = supplied.remove(field)?;
    Some(match value {
        // A string is sent as itself: an argument named `data` that holds
        // `{"a":1}` is a body of `{"a":1}`, not of `"{\"a\":1}"`.
        Value::String(text) => text,
        other => other.to_string(),
    })
}

/// Takes the header arguments out of the arguments.
fn take_headers(
    template: &HttpTransport,
    supplied: &mut Map<String, Value>,
) -> Result<BTreeMap<String, String>, CallError> {
    let mut headers = BTreeMap::new();
    for field in &template.header_fields {
        let Some(value) = supplied.remove(field) else {
            continue;
        };
        let Value::String(text) = value else {
            return Err(CallError::NotAString {
                name: field.clone(),
            });
        };
        headers.insert(field.clone(), text);
    }
    Ok(headers)
}

/// The arguments that are left, as query parameters.
fn remaining_query(supplied: &Map<String, Value>) -> Vec<(String, String)> {
    supplied
        .iter()
        .map(|(name, value)| {
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                Value::Bool(flag) => flag.to_string(),
                other => other.to_string(),
            };
            (name.clone(), encode(&text))
        })
        .collect()
}

/// The credential an HTTP call presents.
fn http_auth(
    auth: Option<&Auth>,
    variables: &BTreeMap<String, String>,
) -> Result<HttpAuth, CallError> {
    match auth {
        None | Some(Auth::Other { .. }) => Ok(HttpAuth::None),
        Some(Auth::ApiKey {
            api_key,
            var_name,
            location,
        }) => Ok(HttpAuth::ApiKey {
            var_name: substitute(var_name, variables)?,
            location: *location,
            value: substitute(api_key, variables)?,
        }),
        Some(Auth::Basic {
            username,
            password,
        }) => Ok(HttpAuth::Basic {
            username: substitute(username, variables)?,
            password: substitute(password, variables)?,
        }),
        Some(Auth::OAuth2 {
            token_url,
            client_id,
            client_secret,
            scope,
        }) => Ok(HttpAuth::OAuth2 {
            token_url: substitute(token_url, variables)?,
            client_id: substitute(client_id, variables)?,
            client_secret: substitute(client_secret, variables)?,
            scope: scope.clone(),
        }),
    }
}

/// Plans an MCP call.
fn plan_mcp(
    tool: &Tool,
    template: &McpTransport,
    arguments: &JsonObject,
    variables: &BTreeMap<String, String>,
) -> Result<McpCall, CallError> {
    let (server_name, tool_name) = split_tool_name(&tool.name, &template.config.servers)?;
    let Some(server) = template.config.servers.get(&server_name) else {
        return Err(CallError::UnknownServer { name: server_name });
    };
    let auth = resolve_mcp_auth(tool.tool_call_template.auth.as_ref(), variables)?;
    Ok(McpCall {
        server_name,
        server: server.clone(),
        tool: tool_name,
        arguments: arguments.clone(),
        auth,
    })
}

/// The credential an MCP call presents.
///
/// The specification applies the template's authentication to an HTTP-based
/// MCP server as the connection's bearer credential, so the block is
/// resolved here and presented by the adapter.
fn resolve_mcp_auth(
    auth: Option<&Auth>,
    variables: &BTreeMap<String, String>,
) -> Result<Option<Auth>, CallError> {
    match auth {
        None => Ok(None),
        Some(Auth::ApiKey {
            api_key,
            var_name,
            location,
        }) => Ok(Some(Auth::ApiKey {
            api_key: substitute(api_key, variables)?,
            var_name: substitute(var_name, variables)?,
            location: *location,
        })),
        Some(Auth::Basic {
            username,
            password,
        }) => Ok(Some(Auth::Basic {
            username: substitute(username, variables)?,
            password: substitute(password, variables)?,
        })),
        Some(Auth::OAuth2 {
            token_url,
            client_id,
            client_secret,
            scope,
        }) => Ok(Some(Auth::OAuth2 {
            token_url: substitute(token_url, variables)?,
            client_id: substitute(client_id, variables)?,
            client_secret: substitute(client_secret, variables)?,
            scope: scope.clone(),
        })),
        Some(other) => Ok(Some(other.clone())),
    }
}

/// Splits a tool's name into the server it belongs to and its name on that
/// server.
fn split_tool_name(
    name: &str,
    servers: &BTreeMap<String, McpServer>,
) -> Result<(String, String), CallError> {
    if let Some((prefix, rest)) = name.split_once('.')
        && servers.contains_key(prefix)
    {
        return Ok((prefix.to_owned(), rest.to_owned()));
    }
    // The name does not name a server. If the template has exactly one, that
    // is the server the call goes to; if it has several, the caller has to
    // say which, and the error names the tool so the message says which.
    if servers.len() == 1 {
        let only = servers.keys().next().cloned().unwrap_or_default();
        return Ok((only, name.to_owned()));
    }
    Err(CallError::AmbiguousServer {
        tool: name.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::utcp::Manual;

    /// Why a test failed.
    #[derive(Debug, PartialEq, Eq)]
    enum Failure {
        /// The manual was refused.
        Manual(crate::domain::utcp::ManualError),
        /// The arguments were not a JSON object.
        Arguments(crate::domain::ir::JsonObjectError),
        /// The plan was refused.
        Plan(CallError),
        /// The manual did not carry the tool the test wanted.
        Missing(&'static str),
    }

    impl From<crate::domain::utcp::ManualError> for Failure {
        fn from(error: crate::domain::utcp::ManualError) -> Self {
            Self::Manual(error)
        }
    }

    impl From<crate::domain::ir::JsonObjectError> for Failure {
        fn from(error: crate::domain::ir::JsonObjectError) -> Self {
            Self::Arguments(error)
        }
    }

    impl From<CallError> for Failure {
        fn from(error: CallError) -> Self {
            Self::Plan(error)
        }
    }

    impl std::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Manual(error) => write!(formatter, "the manual was refused: {error}"),
                Self::Arguments(error) => {
                    write!(formatter, "the arguments were refused: {error}")
                }
                Self::Plan(error) => write!(formatter, "the call was refused: {error}"),
                Self::Missing(what) => write!(formatter, "expected {what}, and it was not there"),
            }
        }
    }

    impl std::error::Error for Failure {}

    /// The HTTP call a manual's tool plans.
    fn http_call(
        template: &str,
        arguments: &str,
    ) -> Result<HttpCall, Failure> {
        http_call_with(template, arguments, &http_policy())
    }

    /// The HTTP call a manual's tool plans, under a given policy.
    fn http_call_with(
        template: &str,
        arguments: &str,
        policy: &crate::domain::utcp::policy::ProtocolPolicy,
    ) -> Result<HttpCall, Failure> {
        let manual = Manual::parse(template)?;
        let Some(tool) = manual.tools.first() else {
            return Err(Failure::Missing("a tool"));
        };
        let arguments = JsonObject::new(arguments)?;
        match plan_call(tool, &arguments, &BTreeMap::new(), policy)? {
            ToolCall::Http(call) => Ok(call),
            ToolCall::Mcp(_) => Err(Failure::Missing("an http call")),
        }
    }

    /// The policy the HTTP tests plan against.
    fn http_policy() -> crate::domain::utcp::policy::ProtocolPolicy {
        crate::domain::utcp::policy::ProtocolPolicy::allowing(["http".to_owned()])
    }

    /// The policy the MCP tests plan against.
    fn mcp_policy() -> crate::domain::utcp::policy::ProtocolPolicy {
        crate::domain::utcp::policy::ProtocolPolicy::allowing(["mcp".to_owned()])
    }

    /// A manual with one HTTP tool.
    fn manual_with(template: &str) -> String {
        format!(
            r#"{{
              "manual_version": "1.0.0",
              "utcp_version": "1.0.1",
              "tools": [{{
                "name": "call",
                "description": "Call the service",
                "inputs": {{"type": "object"}},
                "tool_call_template": {template}
              }}]
            }}"#
        )
    }

    #[test]
    fn a_path_parameter_is_substituted_and_not_sent_again() -> Result<(), Failure> {
        let call = http_call(
            &manual_with(
                r#"{
                  "name": "users",
                  "call_template_type": "http",
                  "url": "https://api.example.com/users/{user_id}",
                  "http_method": "GET"
                }"#,
            ),
            r#"{"user_id": "42", "limit": "10"}"#,
        )?;
        assert_eq!(
            call.url,
            "https://api.example.com/users/42?limit=10".to_owned()
        );
        Ok(())
    }

    #[test]
    fn two_path_parameters_are_both_substituted() -> Result<(), Failure> {
        let call = http_call(
            &manual_with(
                r#"{
                  "name": "posts",
                  "call_template_type": "http",
                  "url": "https://api.example.com/users/{user_id}/posts/{post_id}",
                  "http_method": "GET"
                }"#,
            ),
            r#"{"user_id": "123", "post_id": "456"}"#,
        )?;
        assert_eq!(
            call.url,
            "https://api.example.com/users/123/posts/456".to_owned()
        );
        Ok(())
    }

    #[test]
    fn a_value_is_encoded_down_to_the_unreserved_set() -> Result<(), Failure> {
        let call = http_call(
            &manual_with(
                r#"{
                  "name": "search",
                  "call_template_type": "http",
                  "url": "https://api.example.com/search/{term}",
                  "http_method": "GET"
                }"#,
            ),
            r#"{"term": "a&b/c d"}"#,
        )?;
        assert_eq!(
            call.url,
            "https://api.example.com/search/a%26b%2Fc%20d".to_owned()
        );
        Ok(())
    }

    #[test]
    fn a_body_argument_is_the_body_and_is_not_sent_again() -> Result<(), Failure> {
        let call = http_call(
            &manual_with(
                r#"{
                  "name": "create",
                  "call_template_type": "http",
                  "url": "https://api.example.com/users",
                  "http_method": "POST",
                  "body_field": "data"
                }"#,
            ),
            r#"{"data": {"name": "ada"}, "trace": "1"}"#,
        )?;
        assert_eq!(call.body, Some(r#"{"name":"ada"}"#.to_owned()));
        assert_eq!(call.url, "https://api.example.com/users?trace=1".to_owned());
        Ok(())
    }

    #[test]
    fn a_header_argument_is_a_header_and_is_not_sent_again() -> Result<(), Failure> {
        let call = http_call(
            &manual_with(
                r#"{
                  "name": "read",
                  "call_template_type": "http",
                  "url": "https://api.example.com/users",
                  "http_method": "GET",
                  "header_fields": ["request_id"]
                }"#,
            ),
            r#"{"request_id": "abc", "page": "2"}"#,
        )?;
        assert_eq!(
            call.headers.get("request_id").map(String::as_str),
            Some("abc")
        );
        assert_eq!(call.url, "https://api.example.com/users?page=2".to_owned());
        Ok(())
    }

    #[test]
    fn a_url_that_already_carries_a_query_keeps_it() -> Result<(), Failure> {
        let call = http_call(
            &manual_with(
                r#"{
                  "name": "list",
                  "call_template_type": "http",
                  "url": "https://api.example.com/users?active=true",
                  "http_method": "GET"
                }"#,
            ),
            r#"{"page": "2"}"#,
        )?;
        assert_eq!(
            call.url,
            "https://api.example.com/users?active=true&page=2".to_owned()
        );
        Ok(())
    }

    #[test]
    fn a_missing_path_parameter_is_refused() {
        let refused = http_call(
            &manual_with(
                r#"{
                  "name": "users",
                  "call_template_type": "http",
                  "url": "https://api.example.com/users/{user_id}",
                  "http_method": "GET"
                }"#,
            ),
            r"{}",
        );
        assert_eq!(
            refused,
            Err(Failure::Plan(CallError::MissingArgument {
                name: "user_id".to_owned()
            }))
        );
    }

    #[test]
    fn a_transport_this_build_cannot_call_is_refused() {
        let refused = http_call_with(
            &manual_with(
                r#"{
                  "name": "read",
                  "call_template_type": "file",
                  "file_path": "/etc/tools.json"
                }"#,
            ),
            r"{}",
            &crate::domain::utcp::policy::ProtocolPolicy::allowing([
                "file".to_owned(),
            ]),
        );
        assert_eq!(
            refused,
            Err(Failure::Plan(CallError::UnsupportedTransport {
                call_template_type: "file".to_owned()
            }))
        );
    }

    #[test]
    fn an_api_key_is_resolved_from_the_variables() -> Result<(), Failure> {
        let manual = Manual::parse(&manual_with(
            r#"{
              "name": "read",
              "call_template_type": "http",
              "url": "https://api.example.com/users",
              "http_method": "GET",
              "auth": {
                "auth_type": "api_key",
                "api_key": "${API_KEY}",
                "var_name": "X-API-Key",
                "location": "header"
              }
            }"#,
        ))?;
        let Some(tool) = manual.tools.first() else {
            return Err(Failure::Missing("a tool"));
        };
        let arguments = JsonObject::new("{}")?;
        let mut variables = BTreeMap::new();
        variables.insert("API_KEY".to_owned(), "s3cret".to_owned());
        let call = plan_call(tool, &arguments, &variables, &http_policy())?;
        let ToolCall::Http(call) = call else {
            return Err(Failure::Missing("an http call"));
        };
        assert_eq!(
            call.auth,
            HttpAuth::ApiKey {
                var_name: "X-API-Key".to_owned(),
                location: ApiKeyLocation::Header,
                value: "s3cret".to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn a_variable_that_is_not_set_refuses_the_call() -> Result<(), Failure> {
        let manual = Manual::parse(&manual_with(
            r#"{
              "name": "read",
              "call_template_type": "http",
              "url": "https://api.example.com/users",
              "http_method": "GET",
              "auth": {
                "auth_type": "api_key",
                "api_key": "${API_KEY}",
                "var_name": "X-API-Key",
                "location": "header"
              }
            }"#,
        ))?;
        let Some(tool) = manual.tools.first() else {
            return Err(Failure::Missing("a tool"));
        };
        let arguments = JsonObject::new("{}")?;
        let refused = plan_call(tool, &arguments, &BTreeMap::new(), &http_policy());
        assert!(matches!(
            refused,
            Err(CallError::Variable(crate::domain::utcp::variables::VariableError::Missing {
                ref name
            })) if name == "API_KEY"
        ));
        Ok(())
    }

    #[test]
    fn an_mcp_tool_name_is_split_on_its_server() -> Result<(), Failure> {
        let manual = Manual::parse(
            r#"{
              "manual_version": "1.0.0",
              "utcp_version": "1.0.1",
              "tools": [{
                "name": "filesystem.read_file",
                "description": "Read a file",
                "inputs": {"type": "object"},
                "tool_call_template": {
                  "name": "mcp_tools",
                  "call_template_type": "mcp",
                  "config": {
                    "mcpServers": {
                      "filesystem": {
                        "command": "node",
                        "args": ["mcp-server.js"]
                      }
                    }
                  }
                }
              }]
            }"#,
        )?;
        let Some(tool) = manual.tools.first() else {
            return Err(Failure::Missing("a tool"));
        };
        let arguments = JsonObject::new(r#"{"path": "/tmp/x"}"#)?;
        let call = plan_call(tool, &arguments, &BTreeMap::new(), &mcp_policy())?;
        let ToolCall::Mcp(call) = call else {
            return Err(Failure::Missing("an mcp call"));
        };
        assert_eq!(call.server_name, "filesystem");
        assert_eq!(call.tool, "read_file");
        Ok(())
    }

    #[test]
    fn an_mcp_tool_name_that_names_no_server_goes_to_the_only_server() -> Result<(), Failure> {
        let manual = Manual::parse(
            r#"{
              "manual_version": "1.0.0",
              "utcp_version": "1.0.1",
              "tools": [{
                "name": "report.builder",
                "description": "Build a report",
                "inputs": {"type": "object"},
                "tool_call_template": {
                  "name": "mcp_tools",
                  "call_template_type": "mcp",
                  "config": {
                    "mcpServers": {
                      "only": {"command": "node"}
                    }
                  }
                }
              }]
            }"#,
        )?;
        let Some(tool) = manual.tools.first() else {
            return Err(Failure::Missing("a tool"));
        };
        let arguments = JsonObject::new("{}")?;
        let call = plan_call(tool, &arguments, &BTreeMap::new(), &mcp_policy())?;
        let ToolCall::Mcp(call) = call else {
            return Err(Failure::Missing("an mcp call"));
        };
        assert_eq!(call.server_name, "only");
        assert_eq!(call.tool, "report.builder");
        Ok(())
    }

    #[test]
    fn an_mcp_template_with_several_servers_and_an_unnamed_tool_is_refused() -> Result<(), Failure> {
        let manual = Manual::parse(
            r#"{
              "manual_version": "1.0.0",
              "utcp_version": "1.0.1",
              "tools": [{
                "name": "read",
                "description": "Read",
                "inputs": {"type": "object"},
                "tool_call_template": {
                  "name": "mcp_tools",
                  "call_template_type": "mcp",
                  "config": {
                    "mcpServers": {
                      "one": {"command": "node"},
                      "two": {"command": "python"}
                    }
                  }
                }
              }]
            }"#,
        )?;
        let Some(tool) = manual.tools.first() else {
            return Err(Failure::Missing("a tool"));
        };
        let arguments = JsonObject::new("{}")?;
        let refused = plan_call(tool, &arguments, &BTreeMap::new(), &mcp_policy());
        assert_eq!(
            refused,
            Err(CallError::AmbiguousServer {
                tool: "read".to_owned()
            })
        );
        Ok(())
    }
}
