//! Making the call a manual's tool describes.
//!
//! # What the adapter adds to the plan
//!
//! The application layer decides *what* the request is; this module makes it.
//! Three things happen here that the plan cannot do on its own:
//!
//! - The credential is put where it belongs. An API key in a header, a
//!   query parameter or a cookie; a username and password as Basic; an
//!   `OAuth2` client-credentials exchange, which needs a token before there is
//!   a request at all.
//! - The transport is chosen. An HTTP call is one request; an MCP call is a
//!   session and a `tools/call`, and this crate already knows how to open
//!   both kinds of MCP session.
//! - The response is read. An HTTP call's answer is a body; an MCP call's is
//!   a JSON-RPC result.
//!
//! # Why the token endpoint is checked before it is called
//!
//! The specification says a token endpoint must be HTTPS or a literal
//! loopback address, and that a redirect from it is refused. Both are
//! checked here, before the request goes out: an `OAuth2` token endpoint is a
//! URL from a document this build did not write, and a plain-HTTP one would
//! put the client secret on the wire in the clear.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{Map, Value};

use crate::adapters::secondary::utcp::error::UtcpError;
use crate::adapters::secondary::utcp::token_cache::{TokenCache, TokenKey};
use crate::application::utcp::call::{HttpAuth, HttpCall, McpCall, ToolCall};
use crate::domain::ir::JsonObject;
use crate::domain::utcp::call_template::HttpMethod;
use crate::domain::utcp::{ApiKeyLocation, Auth, McpServer};

/// What a call produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallOutcome {
    /// An HTTP response, as its status and its body.
    Http {
        /// The status code.
        status: u16,
        /// The body, as text.
        body: String,
    },
    /// An MCP result, as JSON.
    Mcp {
        /// The result object.
        result: JsonObject,
    },
}

impl CallOutcome {
    /// The body, as JSON when it is JSON and as text when it is not.
    ///
    /// A caller that wants to print the answer should not have to know
    /// whether the tool answered with JSON or with text, so this is the one
    /// place that decides.
    #[must_use]
    pub fn as_json(&self) -> Value {
        match self {
            Self::Http { body, .. } => {
                serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.clone()))
            }
            Self::Mcp { result } => serde_json::from_str(result.as_str())
                .unwrap_or_else(|_| Value::String(result.as_str().to_owned())),
        }
    }
}

/// Makes the call a plan describes.
///
/// The token cache is created for this call. A caller that makes many calls
/// and wants the tokens shared between them should use
/// [`call_with_cache`] and hand it one cache.
///
/// # Errors
///
/// [`UtcpError::Transport`] when the request fails, [`UtcpError::Status`]
/// when the peer answers with a status that is not a success, and the
/// transport's own errors for a call this build cannot make.
///
/// # Examples
///
/// ```
/// use protocol_droid::adapters::secondary::utcp::call;
/// use protocol_droid::application::utcp::call::{HttpAuth, HttpCall, ToolCall};
/// use protocol_droid::domain::utcp::HttpMethod;
///
/// let plan = ToolCall::Http(HttpCall {
///     method: HttpMethod::Get,
///     url: "https://api.example.com/health".to_owned(),
///     headers: Default::default(),
///     body: None,
///     auth: HttpAuth::None,
/// });
/// // A call that never leaves this machine's test suite.
/// let _ = call(&plan);
/// ```
pub async fn call(call: &ToolCall) -> Result<CallOutcome, UtcpError> {
    call_with_cache(call, &TokenCache::new()).await
}

/// Makes the call a plan describes, sharing tokens through a cache.
///
/// # Errors
///
/// As [`call`].
pub async fn call_with_cache(
    call: &ToolCall,
    cache: &TokenCache,
) -> Result<CallOutcome, UtcpError> {
    match call {
        ToolCall::Http(http) => call_http(http, cache).await,
        ToolCall::Mcp(mcp) => call_mcp(mcp, cache).await,
    }
}

/// Makes an HTTP call.
///
/// # Errors
///
/// As [`call`].
pub async fn call_http(http: &HttpCall, cache: &TokenCache) -> Result<CallOutcome, UtcpError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| UtcpError::Transport {
            detail: error.to_string(),
        })?;
    let mut request = client.request(method_of(&http.method), &http.url);
    for (name, value) in &http.headers {
        request = request.header(name, value);
    }
    if let Some(body) = &http.body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone());
    }
    request = apply_auth(request, &http.auth, cache).await?;

    let response = request.send().await.map_err(|error| UtcpError::Transport {
        detail: error.to_string(),
    })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| UtcpError::Transport {
            detail: error.to_string(),
        })?;
    if !status.is_success() {
        return Err(UtcpError::Status {
            status: status.as_u16(),
        });
    }
    Ok(CallOutcome::Http {
        status: status.as_u16(),
        body,
    })
}

/// The reqwest method for a template's method.
fn method_of(method: &HttpMethod) -> reqwest::Method {
    match method {
        HttpMethod::Get => reqwest::Method::GET,
        HttpMethod::Post => reqwest::Method::POST,
        HttpMethod::Put => reqwest::Method::PUT,
        HttpMethod::Patch => reqwest::Method::PATCH,
        HttpMethod::Delete => reqwest::Method::DELETE,
        HttpMethod::Other(name) => {
            reqwest::Method::from_bytes(name.as_bytes()).unwrap_or(reqwest::Method::GET)
        }
    }
}

/// Puts the credential where the template says it goes.
async fn apply_auth(
    request: reqwest::RequestBuilder,
    auth: &HttpAuth,
    cache: &TokenCache,
) -> Result<reqwest::RequestBuilder, UtcpError> {
    match auth {
        HttpAuth::None => Ok(request),
        HttpAuth::ApiKey {
            var_name,
            location,
            value,
        } => Ok(match location {
            ApiKeyLocation::Header => request.header(var_name, value),
            ApiKeyLocation::Query => request.query(&[(var_name.as_str(), value.as_str())]),
            ApiKeyLocation::Cookie => {
                request.header(reqwest::header::COOKIE, format!("{var_name}={value}"))
            }
        }),
        HttpAuth::Basic { username, password } => {
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
            Ok(request.header(reqwest::header::AUTHORIZATION, format!("Basic {encoded}")))
        }
        HttpAuth::OAuth2 {
            token_url,
            client_id,
            client_secret,
            scope,
        } => {
            let token =
                cached_token(cache, token_url, client_id, client_secret, scope.as_deref()).await?;
            Ok(request.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}")))
        }
    }
}

/// The access token for a set of client credentials.
///
/// The token is fetched at most once per credential configuration, however
/// many requests want it at the same time. The first caller takes the
/// key's lock and does the exchange; the others wait on that lock and find
/// the token already there.
///
/// # Errors
///
/// As [`fetch_token`].
async fn cached_token(
    cache: &TokenCache,
    token_url: &str,
    client_id: &str,
    client_secret: &str,
    scope: Option<&str>,
) -> Result<String, UtcpError> {
    let key = TokenKey::of(token_url, client_id, client_secret, scope);
    cache
        .get_or_fetch(&key, || async {
            fetch_token(token_url, client_id, client_secret, scope).await
        })
        .await
}

/// Exchanges the client credentials for an access token.
///
/// # Errors
///
/// [`UtcpError::TokenEndpoint`] when the endpoint is not one this build will
/// talk to, [`UtcpError::Transport`] when the request fails, and
/// [`UtcpError::Token`] when the endpoint refuses or answers with something
/// that is not a token.
async fn fetch_token(
    token_url: &str,
    client_id: &str,
    client_secret: &str,
    scope: Option<&str>,
) -> Result<String, UtcpError> {
    let url = url::Url::parse(token_url).map_err(|error| UtcpError::Url {
        url: token_url.to_owned(),
        detail: error.to_string(),
    })?;
    check_token_endpoint(&url)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| UtcpError::Transport {
            detail: error.to_string(),
        })?;
    let mut form = vec![
        ("grant_type", "client_credentials"),
        ("client_id", client_id),
        ("client_secret", client_secret),
    ];
    if let Some(scope) = scope {
        form.push(("scope", scope));
    }
    let response = client
        .post(url.as_str())
        .form(&form)
        .send()
        .await
        .map_err(|error| UtcpError::Transport {
            detail: error.to_string(),
        })?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| UtcpError::Transport {
            detail: error.to_string(),
        })?;
    if !status.is_success() {
        return Err(UtcpError::Token { detail: text });
    }
    let value = serde_json::from_str::<Value>(&text).map_err(|error| UtcpError::Token {
        detail: error.to_string(),
    })?;
    let token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| {
            !token.is_empty() && token.chars().all(|character| character.is_ascii_graphic())
        })
        .ok_or_else(|| UtcpError::Token {
            detail: "the answer carried no usable `access_token`".to_owned(),
        })?;
    Ok(token.to_owned())
}

/// Refuses a token endpoint that is neither HTTPS nor a literal loopback
/// address.
fn check_token_endpoint(url: &url::Url) -> Result<(), UtcpError> {
    let scheme = url.scheme();
    let host = url.host_str().unwrap_or_default();
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1");
    if scheme == "https" || (loopback && matches!(scheme, "http" | "ws")) {
        return Ok(());
    }
    Err(UtcpError::TokenEndpoint {
        url: url.as_str().to_owned(),
        reason: "it is neither HTTPS nor a literal loopback address".to_owned(),
    })
}

/// Makes an MCP call.
///
/// The crate's own MCP client does the work: it opens the session, performs
/// the handshake, and calls the tool. What this adds is the mapping from a
/// manual's server entry to the transport the client connects over.
///
/// # Errors
///
/// [`UtcpError::Connect`] when the session cannot be opened,
/// [`UtcpError::Client`] when the call is refused, and
/// [`UtcpError::Spawn`] when a server this build has to start cannot be
/// started.
pub async fn call_mcp(call: &McpCall, cache: &TokenCache) -> Result<CallOutcome, UtcpError> {
    let client = open_client(&call.server, call.auth.as_ref(), cache).await?;
    let result = client
        .call_tool(&call.tool, &call.arguments)
        .await
        .map_err(UtcpError::Client)?;
    Ok(CallOutcome::Mcp { result })
}

/// Opens an MCP session against the server a manual names.
#[cfg(feature = "stdio")]
async fn open_client(
    server: &McpServer,
    auth: Option<&Auth>,
    cache: &TokenCache,
) -> Result<crate::application::client::Client, UtcpError> {
    use crate::adapters::secondary::stdio::ChildSpec;
    use crate::application::client::Client;

    match server {
        McpServer::Http { url, headers, .. } => {
            let spec = crate::domain::TransportSpec::new(
                crate::domain::TransportKind::StreamableHttp,
                url.clone(),
            )
            .map_err(|error| UtcpError::Spec {
                spec: url.clone(),
                detail: error.to_string(),
            })?;
            let _ = headers;
            // The specification: a server entry's own credential wins over
            // the manual-level one. `auth_token` is the entry's own, and an
            // `Authorization` header in the entry's headers is too; the
            // manual-level credential applies only when the entry names
            // neither.
            let mut options = crate::application::client::handler::ConnectOptions::new();
            if let Some(token) = bearer_for(server, auth, cache).await? {
                options = options.authorizing(Arc::new(StaticBearer::new(token)));
            }
            let versions = crate::application::client::default_versions().map_err(|error| {
                UtcpError::Versions {
                    detail: error.to_string(),
                }
            })?;
            Ok(Client::connect_with(&spec, &versions, options).await?)
        }
        McpServer::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let mut spec = ChildSpec::new(command.clone()).map_err(|error| UtcpError::Spawn {
                detail: error.to_string(),
            })?;
            spec = spec
                .with_args(args.clone())
                .map_err(|error| UtcpError::Spawn {
                    detail: error.to_string(),
                })?;
            for (name, value) in env {
                spec = spec
                    .with_env(name.clone(), value.clone())
                    .map_err(|error| UtcpError::Spawn {
                        detail: error.to_string(),
                    })?;
            }
            if let Some(cwd) = cwd {
                spec = spec.with_current_dir(cwd.clone());
            }
            Ok(Client::connect_stdio(&spec)
                .await
                .map_err(|error| UtcpError::Spawn {
                    detail: error.to_string(),
                })?)
        }
        McpServer::Other(_) => Err(UtcpError::Connect(
            crate::application::client::ConnectError::Unsupported {
                kind: crate::domain::TransportKind::Stdio,
                reason: "this build cannot reach the MCP server the manual describes",
            },
        )),
    }
}

/// Opens an MCP session against the server a manual names.
#[cfg(not(feature = "stdio"))]
async fn open_client(
    _server: &McpServer,
    _auth: Option<&Auth>,
    _cache: &TokenCache,
) -> Result<crate::application::client::Client, UtcpError> {
    Err(UtcpError::Connect(
        crate::application::client::connect::ConnectError::Unsupported {
            kind: crate::domain::TransportKind::Stdio,
            reason: "this build was compiled without the `stdio` feature",
        },
    ))
}

/// The JSON body of an MCP `tools/call`, for a caller that wants to see what
/// would go out.
#[must_use]
pub fn mcp_request_body(call: &McpCall) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("name".to_owned(), Value::String(call.tool.clone()));
    let arguments = serde_json::from_str::<Value>(call.arguments.as_str())
        .unwrap_or_else(|_| Value::Object(Map::new()));
    body.insert("arguments".to_owned(), arguments);
    body
}

/// The bearer token an MCP connection presents.
///
/// The specification's precedence, in order:
///
/// 1. The server entry's own `auth_token`.
/// 2. The server entry's own `Authorization` header.
/// 3. The manual-level credential, applied as the connection's bearer.
///
/// A stdio server has no bearer at all: it inherits process security, and a
/// token in the environment is the server's business.
///
/// # Errors
///
/// [`UtcpError::Token`] when the manual-level credential is an `OAuth2`
/// exchange and the token endpoint refuses.
async fn bearer_for(
    server: &McpServer,
    auth: Option<&Auth>,
    cache: &TokenCache,
) -> Result<Option<String>, UtcpError> {
    match server {
        McpServer::Http {
            auth_token,
            headers,
            ..
        } => {
            if let Some(token) = auth_token {
                return Ok(Some(token.clone()));
            }
            if let Some(header) = headers.get("Authorization") {
                return Ok(Some(header.clone()));
            }
            manual_bearer(auth, cache).await
        }
        _ => Ok(None),
    }
}

/// The manual-level credential, as a bearer token.
///
/// # Errors
///
/// [`UtcpError::Token`] when the credential is an `OAuth2` exchange and the
/// token endpoint refuses.
async fn manual_bearer(
    auth: Option<&Auth>,
    cache: &TokenCache,
) -> Result<Option<String>, UtcpError> {
    match auth {
        None => Ok(None),
        Some(Auth::ApiKey {
            api_key,
            var_name,
            location,
        }) => {
            if *location != ApiKeyLocation::Header {
                return Ok(None);
            }
            let _ = var_name;
            Ok(Some(api_key.clone()))
        }
        Some(Auth::Basic { username, password }) => {
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
            Ok(Some(format!("Basic {encoded}")))
        }
        Some(Auth::OAuth2 {
            token_url,
            client_id,
            client_secret,
            scope,
        }) => {
            let token =
                cached_token(cache, token_url, client_id, client_secret, scope.as_deref()).await?;
            Ok(Some(format!("Bearer {token}")))
        }
        Some(Auth::Other { auth_type, .. }) => {
            // An authentication type this build does not model. It is
            // carried through the manual so the document round-trips, but
            // there is no way to present it as a bearer.
            let _ = auth_type;
            Ok(None)
        }
    }
}

/// A bearer token that never changes.
///
/// The specification lets a server entry carry its own `auth_token`. A
/// static token has no refresh flow, so a challenge is a refusal: the
/// client says so rather than retrying with the same token.
#[derive(Debug, Clone)]
struct StaticBearer {
    /// The token.
    token: String,
}

impl StaticBearer {
    /// A bearer that sends `token`.
    const fn new(token: String) -> Self {
        Self { token }
    }
}

impl crate::application::client::bearer::BearerAuth for StaticBearer {
    fn bearer(&self) -> Option<String> {
        Some(self.token.clone())
    }

    fn challenged<'a>(
        &'a self,
        _status: u16,
        _www_authenticate: Option<&'a str>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<(), crate::application::client::bearer::BearerError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async {
            Err(crate::application::client::bearer::BearerError(
                "the server refused the token this server entry carries".to_owned(),
            ))
        })
    }

    fn accepted(&self) {}
}

/// The credential an MCP call presents, as the header it becomes.
#[must_use]
pub fn mcp_auth_header(auth: &Option<Auth>) -> Option<(String, String)> {
    match auth {
        Some(Auth::ApiKey {
            var_name, api_key, ..
        }) => Some((var_name.clone(), api_key.clone())),
        Some(Auth::Basic { username, password }) => Some((
            reqwest::header::AUTHORIZATION.to_string(),
            format!("Basic {username}:{password}"),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::adapters::secondary::utcp::token_cache::TokenCache;
    use crate::domain::utcp::ApiKeyLocation;
    use crate::domain::utcp::Auth;
    use crate::domain::utcp::call_template::McpServer;

    /// An HTTP MCP server entry with no credential of its own.
    fn server_without_credential() -> McpServer {
        McpServer::Http {
            url: "https://api.example.com/mcp".to_owned(),
            headers: BTreeMap::new(),
            auth_token: None,
        }
    }

    /// A server entry that names its own bearer token.
    fn server_with_own_token() -> McpServer {
        McpServer::Http {
            url: "https://api.example.com/mcp".to_owned(),
            headers: BTreeMap::new(),
            auth_token: Some("entry-token".to_owned()),
        }
    }

    /// A server entry that names its own `Authorization` header.
    fn server_with_own_header() -> McpServer {
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_owned(), "Bearer header-token".to_owned());
        McpServer::Http {
            url: "https://api.example.com/mcp".to_owned(),
            headers,
            auth_token: None,
        }
    }

    /// The entry's own token wins over the manual-level credential.
    #[tokio::test]
    async fn a_servers_own_token_wins_over_the_manual_level_one() {
        let cache = TokenCache::new();
        let manual_level = Auth::ApiKey {
            api_key: "manual-key".to_owned(),
            var_name: "X-Key".to_owned(),
            location: ApiKeyLocation::Header,
        };
        let bearer = super::bearer_for(&server_with_own_token(), Some(&manual_level), &cache)
            .await
            .ok()
            .flatten();
        assert_eq!(bearer.as_deref(), Some("entry-token"));
    }

    /// The entry's own `Authorization` header wins over the manual-level one.
    #[tokio::test]
    async fn a_servers_own_header_wins_over_the_manual_level_one() {
        let cache = TokenCache::new();
        let manual_level = Auth::ApiKey {
            api_key: "manual-key".to_owned(),
            var_name: "X-Key".to_owned(),
            location: ApiKeyLocation::Header,
        };
        let bearer = super::bearer_for(&server_with_own_header(), Some(&manual_level), &cache)
            .await
            .ok()
            .flatten();
        assert_eq!(bearer.as_deref(), Some("Bearer header-token"));
    }

    /// With neither, the manual-level credential applies.
    #[tokio::test]
    async fn the_manual_level_credential_applies_when_the_entry_names_none() {
        let cache = TokenCache::new();
        let manual_level = Auth::ApiKey {
            api_key: "manual-key".to_owned(),
            var_name: "X-Key".to_owned(),
            location: ApiKeyLocation::Header,
        };
        let bearer = super::bearer_for(&server_without_credential(), Some(&manual_level), &cache)
            .await
            .ok()
            .flatten();
        assert_eq!(bearer.as_deref(), Some("manual-key"));
    }

    /// No credential anywhere means no bearer.
    #[tokio::test]
    async fn no_credential_anywhere_means_no_bearer() {
        let cache = TokenCache::new();
        let bearer = super::bearer_for(&server_without_credential(), None, &cache)
            .await
            .ok()
            .flatten();
        assert!(bearer.is_none());
    }

    /// A basic credential becomes a basic bearer.
    #[tokio::test]
    async fn a_basic_credential_becomes_a_basic_bearer() {
        let cache = TokenCache::new();
        let manual_level = Auth::Basic {
            username: "user".to_owned(),
            password: "password".to_owned(),
        };
        let bearer = super::bearer_for(&server_without_credential(), Some(&manual_level), &cache)
            .await
            .ok()
            .flatten();
        assert_eq!(bearer.as_deref(), Some("Basic dXNlcjpwYXNzd29yZA=="));
    }

    /// A stdio server has no bearer: it inherits process security.
    #[tokio::test]
    async fn a_stdio_server_has_no_bearer() {
        let cache = TokenCache::new();
        let manual_level = Auth::ApiKey {
            api_key: "manual-key".to_owned(),
            var_name: "X-Key".to_owned(),
            location: ApiKeyLocation::Header,
        };
        let server = McpServer::Stdio {
            command: "node".to_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
        };
        let bearer = super::bearer_for(&server, Some(&manual_level), &cache)
            .await
            .ok()
            .flatten();
        assert!(bearer.is_none());
    }
}
